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

use super::Applied;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

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

    /// Overwrite the journal with the durable steps currently on the stack.
    pub fn write<'a>(&self, steps: impl Iterator<Item = &'a Applied>) {
        let steps: Vec<&Applied> = steps.collect();
        if steps.is_empty() {
            self.clear();
            return;
        }
        let json = match serde_json::to_vec_pretty(&steps) {
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

    /// Read a journal left by a previous process. Returns an empty vec if there is nothing to do,
    /// including when the file is unreadable or corrupt — recovery is best-effort by definition.
    pub fn read_orphaned(&self) -> Vec<Applied> {
        let raw = match std::fs::read(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                warn!(error = %e, "failed to read rollback journal");
                return Vec::new();
            }
        };
        match serde_json::from_slice::<Vec<Applied>>(&raw) {
            Ok(steps) => {
                if !steps.is_empty() {
                    info!(
                        count = steps.len(),
                        "found rollback steps from a previous run"
                    );
                }
                steps
            }
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

/// Write `bytes` to `path` atomically and readable only by the owner.
///
/// The content goes to a temp file in the same directory, is synced, and is then renamed over the
/// destination, so a reader — including the next start after a crash — sees either the old journal
/// or the new one, never a partial write.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "journal path has no parent directory",
        )
    })?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(".rollback-").suffix(".tmp");
    #[cfg(unix)]
    builder
        .permissions(<std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600));
    let mut tmp = builder.tempfile_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vpn::platform::DnsSnapshot;
    use crate::vpn::protocol::InterfaceName;
    use crate::vpn::rollback::{Evidence, Step};

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

    #[test]
    fn missing_journal_reads_as_nothing_to_do() {
        let (_dir, journal) = tmp();
        assert!(journal.read_orphaned().is_empty());
    }

    #[test]
    fn steps_survive_a_write_read_cycle() {
        let (_dir, journal) = tmp();
        let step = dns_step();
        journal.write([&step].into_iter());

        let read = journal.read_orphaned();
        assert_eq!(read, vec![step]);
    }

    #[test]
    fn writing_an_empty_stack_removes_the_file() {
        let (_dir, journal) = tmp();
        let step = dns_step();
        journal.write([&step].into_iter());
        journal.write(std::iter::empty());
        assert!(journal.read_orphaned().is_empty());
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
        journal.write([&dns_step()].into_iter());
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
        journal.write([&dns_step()].into_iter());

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
