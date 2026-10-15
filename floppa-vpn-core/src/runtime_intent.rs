//! One serial runtime writer shared by every deployment. No credentials or preferences.
use crate::{
    autostart::{BUNDLE_FILENAME, BUNDLE_VERSION, LastIntent},
    protocol::Protocol,
};
use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
};
use tokio::sync::oneshot;

enum Change {
    Intent { up: bool, routes: Vec<String> },
    Winner(Protocol),
    Clear,
}
struct Write {
    change: Change,
    reply: oneshot::Sender<Result<(), String>>,
}
pub(crate) struct RuntimeWriter(mpsc::Sender<Write>);

impl RuntimeWriter {
    pub fn open(
        dir: PathBuf,
        boot: Option<String>,
        guard: Arc<dyn Send + Sync>,
    ) -> Result<(Self, Option<LastIntent>), String> {
        let previous = match std::fs::read(dir.join(BUNDLE_FILENAME)) {
            Ok(bytes) => {
                let raw: serde_json::Value = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("invalid runtime state: {e}"))?;
                let version = raw["version"]
                    .as_u64()
                    .ok_or_else(|| "invalid runtime state: missing numeric version".to_string())?;
                if version != u64::from(BUNDLE_VERSION) {
                    None
                } else {
                    Some(
                        serde_json::from_value(raw)
                            .map_err(|e| format!("invalid runtime state: {e}"))?,
                    )
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("cannot read runtime state: {e}")),
        };
        let mut current = previous
            .clone()
            .unwrap_or_else(|| LastIntent::new(vec![], Default::default(), 0));
        let (tx, rx) = mpsc::channel::<Write>();
        std::thread::Builder::new()
            .name("runtime-state".into())
            .spawn(move || {
                let _guard = guard;
                while let Ok(write) = rx.recv() {
                    let mut next = current.clone();
                    match write.change {
                        Change::Intent { up, routes } => {
                            next.desired_up = up;
                            if up {
                                next.private_routes = routes;
                            }
                        }
                        Change::Winner(protocol) => next.last_successful = Some(protocol),
                        Change::Clear => next = LastIntent::new(vec![], Default::default(), 0),
                    }
                    next.boot_id = boot.clone();
                    let result = if next == current {
                        Ok(())
                    } else {
                        next.saved_at = chrono::Utc::now().timestamp();
                        crate::autostart::save(&dir, &next)
                    };
                    if result.is_ok() {
                        current = next;
                    }
                    if let Err(error) = &result {
                        tracing::error!(%error, "runtime state write failed");
                    }
                    let _ = write.reply.send(result);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok((Self(tx), previous))
    }
    fn send(&self, change: Change) -> Result<oneshot::Receiver<Result<(), String>>, String> {
        let (reply, rx) = oneshot::channel();
        self.0
            .send(Write { change, reply })
            .map_err(|_| "runtime writer stopped".to_string())?;
        Ok(rx)
    }
    pub async fn intent(&self, up: bool, routes: Vec<String>) -> Result<(), String> {
        self.send(Change::Intent { up, routes })?
            .await
            .map_err(|_| "runtime writer stopped".to_string())?
    }
    pub fn winner(&self, protocol: Protocol) {
        let _ = self.send(Change::Winner(protocol));
    }
    pub fn clear(&self) {
        let _ = self.send(Change::Clear);
    }
}

pub(crate) fn should_resume(
    previous: Option<&LastIntent>,
    boot: Option<&str>,
    connect_on_boot: bool,
) -> bool {
    match (previous, boot) {
        (Some(previous), Some(boot)) if previous.boot_id.as_deref() == Some(boot) => {
            previous.desired_up
        }
        (_, Some(_)) => connect_on_boot,
        _ => false, // Unknown boot identity must not masquerade as a same-boot restart.
    }
}

pub(crate) async fn boot_id() -> Option<String> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }
    #[cfg(target_os = "windows")]
    {
        // OS-provided boot timestamp, not wall-clock minus sampled uptime.
        let mut cmd = tokio::process::Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", "(Get-CimInstance Win32_OperatingSystem -ErrorAction Stop).LastBootUpTime.ToUniversalTime().Ticks"]).creation_flags(0x08000000).kill_on_drop(true);
        let output = tokio::time::timeout(std::time::Duration::from_secs(5), cmd.output())
            .await
            .ok()?
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout)
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| s.parse::<u64>().is_ok())
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "windows")))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_runtime_is_preserved_and_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(BUNDLE_FILENAME);
        for bytes in ["{}", "{\"version\":\"4\"}", "{\"version\":4}"] {
            std::fs::write(&path, bytes).unwrap();
            assert!(RuntimeWriter::open(dir.path().into(), None, Arc::new(())).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
        }
    }
    #[tokio::test]
    async fn winner_cannot_resurrect_disconnect_and_temporary_is_not_restored() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, _) =
            RuntimeWriter::open(dir.path().into(), Some("boot".into()), Arc::new(())).unwrap();
        writer
            .intent(true, vec!["10.0.0.0/8".into()])
            .await
            .unwrap();
        writer.winner(Protocol::WireGuard);
        writer.intent(false, vec![]).await.unwrap();
        let state = crate::autostart::load(dir.path()).unwrap();
        assert!(!state.desired_up);
        assert_eq!(state.last_successful, Some(Protocol::WireGuard));
        assert_eq!(state.private_routes, ["10.0.0.0/8"]);
        assert_eq!(state.boot_id.as_deref(), Some("boot"));
        writer.winner(Protocol::AmneziaWg);
        writer.intent(false, vec![]).await.unwrap();
        assert!(!crate::autostart::load(dir.path()).unwrap().desired_up);
    }
    #[tokio::test]
    async fn failed_checkpoint_is_reported_and_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("state");
        std::fs::create_dir(&target).unwrap();
        let (writer, _) =
            RuntimeWriter::open(target.clone(), Some("boot".into()), Arc::new(())).unwrap();
        std::fs::remove_dir(&target).unwrap();
        std::fs::write(&target, "obstruction").unwrap();
        assert!(writer.intent(true, vec![]).await.is_err());
        std::fs::remove_file(&target).unwrap();
        std::fs::create_dir(&target).unwrap();
        writer.intent(true, vec![]).await.unwrap();
        assert!(crate::autostart::load(&target).unwrap().desired_up);
    }
    #[test]
    fn boot_policy_and_crash_recovery_are_independent() {
        let mut state = LastIntent::new(vec![Protocol::WireGuard], Default::default(), 0);
        state.boot_id = Some("boot-a".into());
        state.desired_up = true;
        assert!(should_resume(Some(&state), Some("boot-a"), false));
        assert!(!should_resume(Some(&state), Some("boot-b"), false));
        assert!(should_resume(Some(&state), Some("boot-b"), true));
        state.desired_up = false;
        assert!(!should_resume(Some(&state), Some("boot-a"), true));
        assert!(!should_resume(Some(&state), None, true));
    }
}
