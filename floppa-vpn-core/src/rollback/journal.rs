//! On-disk record of the rollback steps that outlive the process.
//!
//! Routes, the endpoint host route and DNS survive a `kill -9`; nothing else does. If the app dies
//! mid-connect, the next start reads this file and unwinds what was left behind, so a crash cannot
//! strand a machine with VPN routes and a rewritten `/etc/resolv.conf` pointing at a tunnel that
//! no longer exists.
//!
//! Every failure here is non-fatal by design: a journal that cannot be written, read or parsed
//! must never prevent a connect. A missing journal degrades to today's behaviour.
//!
//! Writes are atomic (temp file, fsync, rename), so a crash mid-write leaves the previous journal
//! rather than a truncated one. A journal that still fails to parse is moved aside as
//! `rollback.json.corrupt` rather than deleted — it is the only record of what was applied.
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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, info, warn};

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
    stack: StackId,
    #[serde(flatten)]
    applied: Applied,
}

#[derive(Debug, Clone)]
pub struct Journal {
    path: PathBuf,
}

impl Journal {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Where the journal lives, given the app config directory.
    pub fn default_path(config_dir: &Path) -> PathBuf {
        config_dir.join("rollback.json")
    }

    /// Replace `stack`'s records with `steps`, leaving every other stack's records as they are.
    pub fn write<'a>(&self, stack: StackId, steps: impl Iterator<Item = &'a Applied>) {
        let mut records: Vec<Record> = self
            .records()
            .into_iter()
            .filter(|r| r.stack != stack)
            .collect();
        records.extend(steps.map(|applied| Record {
            stack,
            applied: applied.clone(),
        }));
        self.write_records(&records);
    }

    /// Replace the *whole* journal with `steps`, owned by `stack`.
    ///
    /// For a stack rebuilt from everything the journal held: those records are its now, whatever
    /// stack wrote them, and keeping the originals would have them recovered — and undone — twice.
    pub fn claim<'a>(&self, stack: StackId, steps: impl Iterator<Item = &'a Applied>) {
        let records: Vec<Record> = steps
            .map(|applied| Record {
                stack,
                applied: applied.clone(),
            })
            .collect();
        self.write_records(&records);
    }

    fn write_records(&self, records: &[Record]) {
        if records.is_empty() {
            self.clear();
            return;
        }
        let json = match serde_json::to_vec_pretty(records) {
            Ok(j) => j,
            Err(e) => {
                warn!(error = %e, "failed to serialise rollback journal");
                return;
            }
        };
        if let Err(e) = write_private(&self.path, &json) {
            warn!(error = %e, path = %self.path.display(), "failed to write rollback journal");
        }
    }

    pub fn clear(&self) {
        match std::fs::remove_file(&self.path) {
            Ok(()) => debug!("rollback journal cleared"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!(error = %e, "failed to clear rollback journal"),
        }
    }

    /// Read every step left by a previous process, whichever stack wrote it. Returns an empty vec
    /// if there is nothing to do, including when the file is unreadable or corrupt — recovery is
    /// best-effort by definition.
    pub fn read_orphaned(&self) -> Vec<Applied> {
        let steps: Vec<Applied> = self.records().into_iter().map(|r| r.applied).collect();
        if !steps.is_empty() {
            info!(
                count = steps.len(),
                "found rollback steps from a previous run"
            );
        }
        steps
    }

    fn records(&self) -> Vec<Record> {
        let raw = match std::fs::read(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                warn!(error = %e, "failed to read rollback journal");
                return Vec::new();
            }
        };
        match serde_json::from_slice::<Vec<Record>>(&raw) {
            Ok(records) => records,
            Err(e) => {
                warn!(error = %e, "rollback journal is corrupt, moving it aside");
                self.quarantine();
                Vec::new()
            }
        }
    }

    /// Where a corrupt journal is moved to.
    pub fn corrupt_path(&self) -> PathBuf {
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(".corrupt");
        self.path.with_file_name(name)
    }

    fn quarantine(&self) {
        let dest = self.corrupt_path();
        match std::fs::rename(&self.path, &dest) {
            Ok(()) => info!(path = %dest.display(), "corrupt rollback journal kept for inspection"),
            Err(e) => {
                warn!(error = %e, "failed to move the corrupt journal aside; removing it");
                self.clear();
            }
        }
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
    fn missing_journal_reads_as_nothing_to_do() {
        let (_dir, journal) = tmp();
        assert!(journal.read_orphaned().is_empty());
    }

    #[test]
    fn steps_survive_a_write_read_cycle() {
        let (_dir, journal) = tmp();
        let step = dns_step();
        journal.write(StackId::next(), [&step].into_iter());

        let read = journal.read_orphaned();
        assert_eq!(read, vec![step]);
    }

    #[test]
    fn writing_an_empty_stack_removes_the_file() {
        let (_dir, journal) = tmp();
        let step = dns_step();
        let stack = StackId::next();
        journal.write(stack, [&step].into_iter());
        journal.write(stack, std::iter::empty());
        assert!(journal.read_orphaned().is_empty());
    }

    #[test]
    fn a_stacks_write_leaves_other_stacks_records_alone() {
        // The residue of an unwind that gave up must not be erased by the next attempt's stack
        // persisting its own — empty or otherwise — set of durable steps.
        let (_dir, journal) = tmp();
        let (first, second) = (StackId::next(), StackId::next());
        journal.write(first, [&routes_step()].into_iter());

        journal.write(second, std::iter::empty());
        assert_eq!(journal.read_orphaned(), vec![routes_step()]);

        journal.write(second, [&dns_step()].into_iter());
        assert_eq!(journal.read_orphaned(), vec![routes_step(), dns_step()]);

        journal.write(second, std::iter::empty());
        assert_eq!(journal.read_orphaned(), vec![routes_step()]);
    }

    #[test]
    fn a_claim_takes_over_every_record() {
        let (_dir, journal) = tmp();
        journal.write(StackId::next(), [&routes_step()].into_iter());
        journal.write(StackId::next(), [&dns_step()].into_iter());

        let owner = StackId::next();
        let all = journal.read_orphaned();
        journal.claim(owner, all.iter());
        // Now one write by the owner governs all of it.
        journal.write(owner, std::iter::empty());
        assert!(journal.read_orphaned().is_empty());
    }

    #[test]
    fn a_journal_written_before_records_had_stack_ids_still_reads() {
        let (dir, journal) = tmp();
        let old = serde_json::to_vec(&vec![dns_step()]).unwrap();
        std::fs::write(Journal::default_path(dir.path()), old).unwrap();
        assert_eq!(journal.read_orphaned(), vec![dns_step()]);
    }

    #[test]
    fn a_corrupt_journal_is_moved_aside_rather_than_propagated() {
        let (dir, journal) = tmp();
        std::fs::write(Journal::default_path(dir.path()), b"{ not json").unwrap();

        assert!(journal.read_orphaned().is_empty());
        // ...it does not come back to haunt the next read...
        assert!(!Journal::default_path(dir.path()).exists());
        // ...but it is kept, because it is the only record of what was applied.
        assert_eq!(
            std::fs::read(journal.corrupt_path()).unwrap(),
            b"{ not json"
        );
    }

    #[test]
    fn a_write_leaves_no_temp_file_behind() {
        let (dir, journal) = tmp();
        journal.write(StackId::next(), [&dns_step()].into_iter());
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
        journal.write(StackId::next(), [&dns_step()].into_iter());

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
