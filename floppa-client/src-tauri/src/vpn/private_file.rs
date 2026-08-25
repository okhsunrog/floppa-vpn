//! Writing a small file that only this app may read, without ever leaving a half-written one.
//!
//! Two files on the device hold the same private key as the keyring does — the rollback journal
//! and the autostart bundle — and both are read by a process that starts *after* something went
//! wrong: a crash, a boot, an always-on start. A truncate-then-write leaves a window where that
//! reader finds a valid, empty, or half-written file and cannot tell which, and the reader's only
//! recourse is to give up. The journal already wrote through a temp file and a rename; the bundle
//! did not, and a bundle that does not parse is a lockdown device with no network until somebody
//! opens the app.

use std::io::Write;
use std::path::Path;

/// Write `bytes` to `path` atomically, readable only by the owner.
///
/// The content goes to a temp file in the same directory, is synced, and is then renamed over the
/// destination — so a reader sees either the old content or the new one, never a partial write.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path has no parent directory",
        )
    })?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(".floppa-").suffix(".tmp");
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

    #[test]
    fn a_rewrite_replaces_the_content_and_keeps_the_file_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.json");

        write_private(&path, b"first").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");

        // The second write goes through a fresh temp file, so it cannot widen or truncate the
        // one that is already there.
        write_private(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        // Nothing is left behind for the next reader to trip over.
        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name() != "secret.json")
            .collect();
        assert!(strays.is_empty(), "temp files must not survive the write");
    }
}
