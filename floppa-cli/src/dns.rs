//! DNS for the connection.
//!
//! Two backends:
//! - systemd-resolved owns `/etc/resolv.conf` (a symlink into `/run/systemd/resolve/`, or the
//!   `127.0.0.53` stub): the servers are set on the TUN interface with `resolvectl`, and
//!   `resolvectl revert` undoes it. Writing the file in that setup would go through the symlink
//!   into resolved's own stub file.
//! - otherwise `/etc/resolv.conf` is moved aside (rename keeps a symlink a symlink) and
//!   replaced. The original is also kept in memory, so the restore works even if the on-disk
//!   backup is gone, and a backup left behind by a process that died is recognised as the
//!   original rather than overwritten.

use anyhow::{Context, Result, bail};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

const RESOLV_CONF: &str = "/etc/resolv.conf";
const RESOLV_BACKUP: &str = "/etc/resolv.conf.floppa-backup";

/// What `restore` has to undo.
pub enum DnsBackup {
    /// Servers were set on `interface` through systemd-resolved.
    Resolvectl { interface: String },
    /// `/etc/resolv.conf` was replaced; this is what it was.
    ResolvConf(ResolvConfSnapshot),
}

/// The original `/etc/resolv.conf`: its symlink target if it was a symlink, and its content.
pub struct ResolvConfSnapshot {
    symlink_target: Option<PathBuf>,
    content: Option<String>,
}

/// Point the system resolver at `servers` for the lifetime of the connection.
/// On failure nothing is left changed.
pub fn apply(interface: &str, servers: &[String]) -> Result<DnsBackup> {
    if servers.is_empty() {
        bail!("No DNS servers to apply");
    }

    let original = ResolvConfSnapshot::capture();

    if original.is_systemd_resolved() {
        match apply_resolvectl(interface, servers) {
            Ok(()) => {
                eprintln!(
                    "DNS: {} (systemd-resolved, {interface})",
                    servers.join(", ")
                );
                return Ok(DnsBackup::Resolvectl {
                    interface: interface.to_string(),
                });
            }
            Err(e) => {
                eprintln!("resolvectl failed ({e:#}); replacing {RESOLV_CONF} instead");
                let _ = resolvectl(&["revert", interface]);
            }
        }
    }

    if let Err(e) = original.replace_with(servers) {
        if let Err(restore) = original.restore() {
            eprintln!("Failed to put {RESOLV_CONF} back: {restore:#}");
        }
        return Err(e);
    }
    eprintln!("DNS: {}", servers.join(", "));
    Ok(DnsBackup::ResolvConf(original))
}

impl DnsBackup {
    pub fn restore(&self) -> Result<()> {
        match self {
            DnsBackup::Resolvectl { interface } => resolvectl(&["revert", interface])?,
            DnsBackup::ResolvConf(snapshot) => snapshot.restore()?,
        }
        eprintln!("DNS restored.");
        Ok(())
    }
}

fn apply_resolvectl(interface: &str, servers: &[String]) -> Result<()> {
    let mut args = vec!["dns", interface];
    args.extend(servers.iter().map(String::as_str));
    resolvectl(&args)?;
    // Route every lookup to this link, not only its search domains.
    resolvectl(&["domain", interface, "~."])?;
    resolvectl(&["default-route", interface, "true"])
}

fn resolvectl(args: &[&str]) -> Result<()> {
    let output = Command::new("resolvectl")
        .args(args)
        .output()
        .context("Failed to run resolvectl")?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("resolvectl {} failed: {}", args.join(" "), stderr.trim())
    }
}

impl ResolvConfSnapshot {
    /// Snapshot the original resolver config. A backup left by a previous run that died is
    /// the original; the live file is then already the VPN one.
    fn capture() -> Self {
        let backup = Path::new(RESOLV_BACKUP);
        let source = if backup.symlink_metadata().is_ok() {
            eprintln!("Found {RESOLV_BACKUP} from a previous run, treating it as the original");
            backup
        } else {
            Path::new(RESOLV_CONF)
        };
        let symlink_target = fs::symlink_metadata(source)
            .ok()
            .filter(|m| m.file_type().is_symlink())
            .and_then(|_| fs::read_link(source).ok());
        Self {
            symlink_target,
            content: fs::read_to_string(source).ok(),
        }
    }

    fn is_systemd_resolved(&self) -> bool {
        let via_symlink = self
            .symlink_target
            .as_ref()
            .is_some_and(|t| t.to_string_lossy().contains("systemd/resolve"));
        let via_stub = self.content.as_deref().is_some_and(|c| {
            c.lines().any(|l| {
                let mut words = l.split_whitespace();
                words.next() == Some("nameserver") && words.next() == Some("127.0.0.53")
            })
        });
        via_symlink || via_stub
    }

    /// Move the live file aside (unless a backup already exists) and write the VPN servers.
    fn replace_with(&self, servers: &[String]) -> Result<()> {
        if Path::new(RESOLV_BACKUP).symlink_metadata().is_err() {
            match fs::rename(RESOLV_CONF, RESOLV_BACKUP) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                // A bind-mounted file (containers) cannot be renamed; a copy has to do.
                Err(_) => {
                    fs::copy(RESOLV_CONF, RESOLV_BACKUP)
                        .with_context(|| format!("Failed to back up {RESOLV_CONF}"))?;
                }
            }
        }

        let content: String = servers
            .iter()
            .map(|s| format!("nameserver {s}\n"))
            .collect();
        fs::write(RESOLV_CONF, content).with_context(|| format!("Failed to write {RESOLV_CONF}"))
    }

    /// Put the original back: the on-disk backup by rename when possible, otherwise from memory.
    fn restore(&self) -> Result<()> {
        let backup = Path::new(RESOLV_BACKUP);
        let have_backup = backup.symlink_metadata().is_ok();
        if have_backup && fs::rename(backup, RESOLV_CONF).is_ok() {
            return Ok(());
        }

        let result = match (&self.symlink_target, &self.content) {
            // Recreate the link; if the file cannot be removed (bind mount) write in place.
            (Some(target), content) => match fs::remove_file(RESOLV_CONF) {
                Ok(()) => std::os::unix::fs::symlink(target, RESOLV_CONF)
                    .with_context(|| format!("Failed to restore {RESOLV_CONF} symlink")),
                Err(_) => match content {
                    Some(content) => fs::write(RESOLV_CONF, content)
                        .with_context(|| format!("Failed to restore {RESOLV_CONF}")),
                    None => bail!("Cannot restore {RESOLV_CONF}: no content saved"),
                },
            },
            (None, Some(content)) => fs::write(RESOLV_CONF, content)
                .with_context(|| format!("Failed to restore {RESOLV_CONF}")),
            (None, None) => match fs::remove_file(RESOLV_CONF) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e).with_context(|| format!("Failed to remove {RESOLV_CONF}")),
            },
        };
        if have_backup && result.is_ok() {
            let _ = fs::remove_file(backup);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(symlink_target: Option<&str>, content: Option<&str>) -> ResolvConfSnapshot {
        ResolvConfSnapshot {
            symlink_target: symlink_target.map(PathBuf::from),
            content: content.map(str::to_string),
        }
    }

    #[test]
    fn detects_systemd_resolved_by_symlink_or_stub() {
        assert!(
            snapshot(Some("../run/systemd/resolve/stub-resolv.conf"), None).is_systemd_resolved()
        );
        assert!(snapshot(Some("/run/systemd/resolve/resolv.conf"), None).is_systemd_resolved());
        assert!(
            snapshot(
                None,
                Some("# Generated\nnameserver 127.0.0.53\noptions edns0\n")
            )
            .is_systemd_resolved()
        );
        assert!(!snapshot(None, Some("nameserver 1.1.1.1\n")).is_systemd_resolved());
        assert!(!snapshot(Some("/etc/resolvconf/run/resolv.conf"), None).is_systemd_resolved());
        assert!(!snapshot(None, None).is_systemd_resolved());
    }
}
