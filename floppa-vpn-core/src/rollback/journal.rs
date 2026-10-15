//! On-disk record of the rollback steps that outlive the process.
//!
//! Routes, the endpoint host route and DNS survive a `kill -9`; nothing else does. If the app dies
//! mid-connect, the next start reads this file and unwinds what was left behind, so a crash cannot
//! strand a machine with VPN routes and a rewritten `/etc/resolv.conf` pointing at a tunnel that
//! no longer exists.
//!
//! Reading and writing are fail-closed: only a missing file means an empty journal. An
//! unreadable or corrupt file stays in place and prevents new durable mutations. Atomic writes
//! preserve the previous record when replacement fails.
//!
//! # One file, several stacks
//!
//! Every record names the [`StackId`] that wrote it, and a stack's write replaces *its own*
//! records only. That is what lets a durable step whose undo failed survive: it stays in the file
//! under the stack that gave up on it, while the next attempt — a new stack, on the same journal —
//! writes its own records alongside. Previously a write was "the journal is now my durable steps",
//! and the next attempt's very first push erased the residue the previous unwind had just
//! deliberately kept, so crash recovery on the next start never saw it.

use super::Applied;
use crate::private_file::write_private;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, info};

/// Which stack a journal record belongs to.
///
/// Minted per process, so ids are unique among the stacks alive in one process — which is all
/// that matters: a stack only ever removes records carrying its own id, and a stack rebuilt from
/// the journal [claims](Journal::claim) every record it read, whatever id a previous process gave
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct StackId(u64);

impl StackId {
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for StackId {
    /// The id of records written before records had ids. Never minted; only ever read.
    fn default() -> Self {
        Self(0)
    }
}

/// One line of the journal: a durable step and the stack it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Record {
    #[serde(default)]
    boot_id: Option<String>,
    #[serde(default)]
    stack: StackId,
    #[serde(flatten)]
    applied: Applied,
}

#[derive(Debug, Clone)]
pub struct Journal {
    path: std::sync::Arc<PathBuf>,
    boot_id: Option<std::sync::Arc<str>>,
}

impl Journal {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: std::sync::Arc::new(path.into()),
            boot_id: None,
        }
    }

    #[cfg(target_os = "windows")]
    pub fn for_boot(path: PathBuf, boot_id: String) -> Self {
        Self {
            path: std::sync::Arc::new(path),
            boot_id: Some(boot_id.into()),
        }
    }

    /// Where the journal lives, given the app config directory.
    pub fn default_path(config_dir: &Path) -> PathBuf {
        config_dir.join("rollback.json")
    }

    /// Replace `stack`'s records with `steps`, leaving every other stack's records as they are.
    pub fn write<'a>(
        &self,
        stack: StackId,
        steps: impl Iterator<Item = &'a Applied>,
    ) -> io::Result<()> {
        let mut records: Vec<Record> = self
            .records()?
            .into_iter()
            .filter(|r| r.stack != stack)
            .collect();
        records.extend(steps.map(|applied| Record {
            boot_id: self.boot_id.as_deref().map(str::to_owned),
            stack,
            applied: applied.clone(),
        }));
        self.write_records(&records)
    }

    /// Replace the *whole* journal with `steps`, owned by `stack`.
    ///
    /// For a stack rebuilt from everything the journal held: those records are its now, whatever
    /// stack wrote them, and keeping the originals would have them recovered — and undone — twice.
    pub fn claim<'a>(
        &self,
        stack: StackId,
        steps: impl Iterator<Item = &'a Applied>,
    ) -> io::Result<()> {
        let records: Vec<Record> = steps
            .map(|applied| Record {
                boot_id: self.boot_id.as_deref().map(str::to_owned),
                stack,
                applied: applied.clone(),
            })
            .collect();
        self.write_records(&records)
    }

    fn write_records(&self, records: &[Record]) -> io::Result<()> {
        if records.is_empty() {
            return self.clear();
        }
        let json = serde_json::to_vec_pretty(records).map_err(io::Error::other)?;
        write_private(&self.path, &json)
    }

    pub fn clear(&self) -> io::Result<()> {
        match std::fs::remove_file(self.path.as_ref()) {
            Ok(()) => {
                crate::private_file::sync_parent(&self.path)?;
                debug!("rollback journal cleared");
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// A missing file is empty; every other failure preserves the evidence and blocks recovery.
    pub fn read_orphaned(&self) -> io::Result<Vec<Applied>> {
        let steps: Vec<Applied> = self.records()?.into_iter().map(|r| r.applied).collect();
        if !steps.is_empty() {
            info!(
                count = steps.len(),
                "found rollback steps from a previous run"
            );
        }
        Ok(steps)
    }

    fn records(&self) -> io::Result<Vec<Record>> {
        let raw = match std::fs::read(self.path.as_ref()) {
            Ok(raw) => raw,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut records: Vec<Record> = serde_json::from_slice(&raw)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if self.boot_id.is_some() {
            // Windows installs these routes only in ActiveStore. They disappeared at reboot;
            // never delete a new owner's coincidentally identical endpoint route.
            records.retain(|record| {
                record.boot_id.as_deref() == self.boot_id.as_deref()
                    || !matches!(
                        record.applied.step,
                        super::Step::EndpointRoute { .. } | super::Step::Routes { .. }
                    )
            });
        }
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::DnsSnapshot;
    use crate::protocol::InterfaceName;
    use crate::rollback::{Evidence, Step};

    fn tmp() -> (tempfile::TempDir, Journal) {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::new(Journal::default_path(dir.path()));
        (dir, journal)
    }

    fn dns_step() -> Applied {
        Applied {
            step: Step::Dns {
                iface: InterfaceName::default(),
                snapshot: DnsSnapshot::Resolvectl,
                if_index: None,
            },
            evidence: Evidence::Done,
        }
    }

    fn routes_step() -> Applied {
        Applied {
            step: Step::Routes {
                iface: InterfaceName::default(),
                routes: vec!["0.0.0.0/1".parse().unwrap()],
                if_index: None,
            },
            evidence: Evidence::Done,
        }
    }

    #[test]
    fn new_boot_does_not_replay_ephemeral_routes_but_keeps_dns_cleanup() {
        let (_dir, mut journal) = tmp();
        journal.boot_id = Some("old".into());
        journal
            .write(StackId::next(), [routes_step(), dns_step()].iter())
            .unwrap();
        assert_eq!(journal.read_orphaned().unwrap().len(), 2);
        journal.boot_id = Some("new".into());
        assert_eq!(journal.read_orphaned().unwrap(), [dns_step()]);
    }

    #[test]
    fn missing_journal_reads_as_nothing_to_do() {
        let (_dir, journal) = tmp();
        assert!(journal.read_orphaned().unwrap().is_empty());
    }

    #[test]
    fn steps_survive_a_write_read_cycle() {
        let (_dir, journal) = tmp();
        let step = dns_step();
        journal.write(StackId::next(), [&step].into_iter()).unwrap();

        let read = journal.read_orphaned().unwrap();
        assert_eq!(read, vec![step]);
    }

    #[test]
    fn writing_an_empty_stack_removes_the_file() {
        let (_dir, journal) = tmp();
        let step = dns_step();
        let stack = StackId::next();
        journal.write(stack, [&step].into_iter()).unwrap();
        journal.write(stack, std::iter::empty()).unwrap();
        assert!(journal.read_orphaned().unwrap().is_empty());
    }

    #[test]
    fn a_stacks_write_leaves_other_stacks_records_alone() {
        // The residue of an unwind that gave up must not be erased by the next attempt's stack
        // persisting its own — empty or otherwise — set of durable steps.
        let (_dir, journal) = tmp();
        let (first, second) = (StackId::next(), StackId::next());
        journal.write(first, [&routes_step()].into_iter()).unwrap();

        journal.write(second, std::iter::empty()).unwrap();
        assert_eq!(journal.read_orphaned().unwrap(), vec![routes_step()]);

        journal.write(second, [&dns_step()].into_iter()).unwrap();
        assert_eq!(
            journal.read_orphaned().unwrap(),
            vec![routes_step(), dns_step()]
        );

        journal.write(second, std::iter::empty()).unwrap();
        assert_eq!(journal.read_orphaned().unwrap(), vec![routes_step()]);
    }

    #[test]
    fn a_claim_takes_over_every_record() {
        let (_dir, journal) = tmp();
        journal
            .write(StackId::next(), [&routes_step()].into_iter())
            .unwrap();
        journal
            .write(StackId::next(), [&dns_step()].into_iter())
            .unwrap();

        let owner = StackId::next();
        let all = journal.read_orphaned().unwrap();
        journal.claim(owner, all.iter()).unwrap();
        // Now one write by the owner governs all of it.
        journal.write(owner, std::iter::empty()).unwrap();
        assert!(journal.read_orphaned().unwrap().is_empty());
    }

    #[test]
    fn a_journal_written_before_records_had_stack_ids_still_reads() {
        let (dir, journal) = tmp();
        let old = serde_json::to_vec(&vec![dns_step()]).unwrap();
        std::fs::write(Journal::default_path(dir.path()), old).unwrap();
        assert_eq!(journal.read_orphaned().unwrap(), vec![dns_step()]);
    }

    #[test]
    fn corrupt_journal_blocks_reads_and_writes_without_losing_evidence() {
        let (dir, journal) = tmp();
        let path = Journal::default_path(dir.path());
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(journal.read_orphaned().is_err());
        assert!(
            journal
                .write(StackId::next(), [&dns_step()].into_iter())
                .is_err()
        );
        assert!(journal.read_orphaned().is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"{ not json");
    }

    #[test]
    fn a_write_leaves_no_temp_file_behind() {
        let (dir, journal) = tmp();
        journal
            .write(StackId::next(), [&dns_step()].into_iter())
            .unwrap();
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["rollback.json".to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn journal_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, journal) = tmp();
        journal
            .write(StackId::next(), [&dns_step()].into_iter())
            .unwrap();

        let mode = std::fs::metadata(Journal::default_path(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "journal must not be group/world accessible"
        );
    }
}
