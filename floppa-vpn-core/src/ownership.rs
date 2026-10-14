//! One machine, one tunnel owner. The lease is held through offline waits and teardown.
//! Files are never unlinked on release: replacing the inode would split ownership.
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum OwnerError {
    #[error("another Floppa process owns the tunnel; stop that GUI/CLI or use the system service")]
    Busy,
    #[error("cannot acquire tunnel ownership: {0}")]
    Io(#[from] std::io::Error),
    #[error("cannot prepare tunnel ownership: {0}")]
    Prepare(String),
}

#[cfg(not(target_os = "windows"))]
#[derive(Debug)]
pub struct OwnerGuard(std::fs::File);

#[cfg(not(target_os = "windows"))]
impl OwnerGuard {
    fn lock(file: std::fs::File) -> Result<Self, OwnerError> {
        match file.try_lock() {
            Ok(()) => Ok(Self(file)),
            Err(std::fs::TryLockError::WouldBlock) => Err(OwnerError::Busy),
            Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
        }
    }

    pub async fn acquire(
        platform: &crate::platform::PlatformImpl,
        dir: &Path,
    ) -> Result<Self, OwnerError> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
            let _ = dir;
            let path = Path::new("/run/floppa-vpn/tunnel.lock");
            if !path.try_exists()? {
                platform
                    .prepare_ownership()
                    .await
                    .map_err(|e| OwnerError::Prepare(e.to_string()))?;
            }
            let parent = std::fs::symlink_metadata(path.parent().unwrap())?;
            if !parent.is_dir() || parent.uid() != 0 || parent.mode() & 0o022 != 0 {
                return Err(OwnerError::Prepare("unsafe ownership directory".into()));
            }
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path)?;
            let meta = file.metadata()?;
            if !meta.is_file() || meta.uid() != 0 || meta.mode() & 0o022 != 0 || meta.nlink() != 1 {
                return Err(OwnerError::Prepare("unsafe ownership file".into()));
            }
            Self::lock(file)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = platform;
            // Android's private app directory is shared by UI/:vpn, not other apps.
            std::fs::create_dir_all(dir)?;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(dir.join("tunnel.lock"))?;
            Self::lock(file)
        }
    }
}

#[cfg(not(target_os = "windows"))]
impl Drop for OwnerGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(target_os = "windows")]
#[derive(Debug)]
pub struct OwnerGuard {
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(target_os = "windows")]
impl OwnerGuard {
    pub async fn acquire(_: &crate::platform::PlatformImpl, _: &Path) -> Result<Self, OwnerError> {
        let (ready, result) = std::sync::mpsc::sync_channel(1);
        let (stop, stopped) = std::sync::mpsc::channel();
        // A Win32 mutex belongs to an OS thread, not a Tokio task. Keep that thread until Drop.
        let thread = std::thread::spawn(move || unsafe {
            use windows_sys::Win32::{
                Foundation::*, Security::Authorization::*, Security::*, System::Threading::*,
            };
            let name: Vec<u16> = "Global\\FloppaVPN.TunnelOwner"
                .encode_utf16()
                .chain(Some(0))
                .collect();
            // Authenticated local users may wait/release, not change the ACL. Another user gets
            // Busy rather than a second per-session mutex. Abandoned ownership is recoverable.
            let sddl: Vec<u16> = "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x00100001;;;AU)"
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let mut descriptor = std::ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                std::ptr::null_mut(),
            ) == 0
            {
                let _ = ready.send(Err(OwnerError::Io(std::io::Error::last_os_error())));
                return;
            }
            let attrs = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            };
            let handle = CreateMutexExW(&attrs, name.as_ptr(), 0, 0x00100001);
            LocalFree(descriptor);
            if handle.is_null() {
                let _ = ready.send(Err(OwnerError::Io(std::io::Error::last_os_error())));
                return;
            }
            match WaitForSingleObject(handle, 0) {
                WAIT_OBJECT_0 | WAIT_ABANDONED => {
                    let _ = ready.send(Ok(()));
                    let _ = stopped.recv();
                    ReleaseMutex(handle);
                }
                WAIT_TIMEOUT => {
                    let _ = ready.send(Err(OwnerError::Busy));
                }
                _ => {
                    let _ = ready.send(Err(OwnerError::Io(std::io::Error::last_os_error())));
                }
            }
            CloseHandle(handle);
        });
        result
            .recv()
            .map_err(|e| OwnerError::Prepare(e.to_string()))??;
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
        })
    }
}

#[cfg(target_os = "windows")]
impl Drop for OwnerGuard {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn lease_excludes_other_descriptors_and_releases_without_unlinking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owner");
        std::fs::write(&path, b"").unwrap();
        let first = OwnerGuard::lock(std::fs::File::open(&path).unwrap()).unwrap();
        assert!(matches!(
            OwnerGuard::lock(std::fs::File::open(&path).unwrap()),
            Err(OwnerError::Busy)
        ));
        drop(first);
        let _next = OwnerGuard::lock(std::fs::File::open(&path).unwrap()).unwrap();
        assert!(path.exists());
    }
    #[test]
    fn subprocess_owner() {
        let Some(path) = std::env::var_os("FLOPPA_TEST_OWNER_FILE") else {
            return;
        };
        let _guard = OwnerGuard::lock(std::fs::File::open(&path).unwrap()).unwrap();
        std::fs::write(
            std::path::PathBuf::from(path).with_extension("ready"),
            b"ready",
        )
        .unwrap();
        loop {
            std::thread::park();
        }
    }

    #[test]
    fn process_death_releases_lease() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owner");
        std::fs::write(&path, b"").unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ownership::tests::subprocess_owner",
                "--nocapture",
            ])
            .env("FLOPPA_TEST_OWNER_FILE", &path)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !path.with_extension("ready").exists() {
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("child did not acquire lease");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let busy = OwnerGuard::lock(std::fs::File::open(&path).unwrap());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(matches!(busy, Err(OwnerError::Busy)));
        let _next = OwnerGuard::lock(std::fs::File::open(path).unwrap()).unwrap();
    }
}
