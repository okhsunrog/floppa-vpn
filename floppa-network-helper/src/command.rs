//! No shell, inherited PATH, grep, or best-effort status interpretation.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    io::Write,
    os::{fd::RawFd, unix::process::CommandExt},
    process::{Command, Stdio},
};

pub struct Commands {
    pub lock_fd: RawFd,
}
impl Commands {
    pub fn run(&self, tool: &str, args: &[&str], input: Option<&str>) -> Result<String> {
        let path = option_env!("FLOPPA_HELPER_PATH").unwrap_or("/usr/sbin:/usr/bin:/sbin:/bin");
        #[cfg(feature = "test-hooks")]
        let path_override = std::env::var("FLOPPA_TEST_PATH").ok();
        #[cfg(feature = "test-hooks")]
        let path = path_override.as_deref().unwrap_or(path);
        let executable = std::env::split_paths(path)
            .map(|d| d.join(tool))
            .find(|p| p.is_file())
            .with_context(|| format!("required tool {tool} is missing"))?;
        let mut cmd = Command::new(executable);
        cmd.args(args)
            .env_clear()
            .env("PATH", path)
            .env("LC_ALL", "C")
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let fd = self.lock_fd;
        // Keep the journal lock held by a mutating child if this helper is killed. Otherwise a
        // second helper could start undo while the orphaned ip/nft process is still applying.
        // SAFETY: pre_exec uses only async-signal-safe fcntl; fd belongs to the live Journal.
        unsafe {
            cmd.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().with_context(|| format!("start {tool}"))?;
        let write_result = if let Some(input) = input {
            child
                .stdin
                .take()
                .context("child stdin")?
                .write_all(input.as_bytes())
        } else {
            Ok(())
        };
        let output = child
            .wait_with_output()
            .with_context(|| format!("wait for {tool}"))?;
        ensure!(
            output.status.success(),
            "{tool} {} failed ({}): {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        write_result?;
        String::from_utf8(output.stdout).with_context(|| format!("invalid UTF-8 from {tool}"))
    }
    pub fn json(&self, tool: &str, args: &[&str]) -> Result<Value> {
        serde_json::from_str(&self.run(tool, args, None)?)
            .with_context(|| format!("invalid JSON from {tool}"))
    }
    pub fn ip(&self, args: &[&str]) -> Result<()> {
        self.run("ip", args, None)?;
        crate::checkpoint("ip");
        Ok(())
    }
}
