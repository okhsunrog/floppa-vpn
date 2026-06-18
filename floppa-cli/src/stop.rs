#[cfg(unix)]
use anyhow::Context;
#[cfg(unix)]
use anyhow::anyhow;
use anyhow::{Result, bail};
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process;
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
use crate::tunnel;

#[cfg(unix)]
#[derive(Debug, Clone)]
struct ConnectProcess {
    pid: u32,
    pgid: u32,
}

pub fn stop(interface: &str, pid: Option<u32>, force: bool) -> Result<()> {
    #[cfg(unix)]
    {
        stop_unix(interface, pid, force)
    }

    #[cfg(not(unix))]
    {
        let _ = (interface, pid, force);
        bail!("floppa-cli stop is only supported on Unix-like systems")
    }
}

#[cfg(unix)]
fn stop_unix(interface: &str, pid: Option<u32>, force: bool) -> Result<()> {
    if !tunnel::interface_exists(interface) {
        println!("Floppa {interface}: not connected");
        return Ok(());
    }

    let current_pid = process::id();
    let candidates = find_connect_processes(current_pid)?;
    let target = match pid {
        Some(pid) => candidates
            .into_iter()
            .find(|process| process.pid == pid)
            .ok_or_else(|| anyhow!("floppa-cli connect process {pid} was not found"))?,
        None if candidates.len() == 1 => candidates
            .into_iter()
            .next()
            .expect("single candidate checked above"),
        None if candidates.len() > 1 => {
            let pids = candidates
                .iter()
                .map(|process| format!("pid={} pgid={}", process.pid, process.pgid))
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "Found multiple floppa-cli connect processes: {}. Re-run with --pid <pid>.",
                pids
            );
        }
        None => {
            bail!(
                "Floppa {interface} interface exists, but no running floppa-cli connect process was found"
            );
        }
    };

    eprintln!(
        "Stopping Floppa {interface} via pid={} pgid={}...",
        target.pid, target.pgid
    );
    signal(&target, Signal::Terminate)?;

    if wait_until_disconnected(interface, target.pid, Duration::from_secs(15)) {
        println!("Floppa {interface}: disconnected");
        return Ok(());
    }

    if force {
        eprintln!("Timed out; sending SIGKILL to pid={}...", target.pid);
        signal(&target, Signal::Kill)?;
        if wait_until_disconnected(interface, target.pid, Duration::from_secs(10)) {
            println!("Floppa {interface}: disconnected");
            return Ok(());
        }
    }

    bail!(
        "Floppa {interface} still exists after stop. Re-run with --force or inspect pid {}.",
        target.pid
    )
}

#[cfg(unix)]
fn wait_until_disconnected(interface: &str, pid: u32, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        if !tunnel::interface_exists(interface) {
            return true;
        }
        if start.elapsed() >= timeout || !process_exists(pid) {
            return false;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum Signal {
    Terminate,
    Kill,
}

impl Signal {
    fn as_raw(self) -> i32 {
        match self {
            Self::Terminate => libc::SIGTERM,
            Self::Kill => libc::SIGKILL,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Terminate => "SIGTERM",
            Self::Kill => "SIGKILL",
        }
    }
}

#[cfg(unix)]
fn signal(process: &ConnectProcess, signal: Signal) -> Result<()> {
    let target = if process.pgid == process.pid {
        -(process.pgid as i32)
    } else {
        process.pid as i32
    };

    let rc = unsafe { libc::kill(target, signal.as_raw()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(anyhow!(
            "failed to send {} to pid={} pgid={}: {}",
            signal.name(),
            process.pid,
            process.pgid,
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(unix)]
fn find_connect_processes(current_pid: u32) -> Result<Vec<ConnectProcess>> {
    let mut processes = Vec::new();

    for entry in fs::read_dir("/proc").context("Failed to read /proc")? {
        let entry = entry?;
        let Some(pid) = parse_proc_pid(&entry.path()) else {
            continue;
        };
        if pid == current_pid {
            continue;
        }

        let Some(pgid) = read_pgid(pid)? else {
            continue;
        };
        let Some(cmdline) = read_cmdline(pid)? else {
            continue;
        };

        if is_floppa_connect_cmdline(&cmdline) {
            processes.push(ConnectProcess { pid, pgid });
        }
    }

    processes.sort_by_key(|process| process.pid);
    Ok(processes)
}

#[cfg(unix)]
fn parse_proc_pid(path: &Path) -> Option<u32> {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.parse().ok())
}

#[cfg(unix)]
fn read_pgid(pid: u32) -> Result<Option<u32>> {
    let stat = match fs::read_to_string(proc_path(pid, "stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to read /proc/{pid}/stat"));
        }
    };

    let Some(after_comm) = stat.rsplit_once(')') else {
        return Ok(None);
    };
    let fields = after_comm.1.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 3 {
        return Ok(None);
    }

    fields[2]
        .parse::<u32>()
        .map(Some)
        .with_context(|| format!("Failed to parse process group for pid {pid}"))
}

#[cfg(unix)]
fn read_cmdline(pid: u32) -> Result<Option<String>> {
    let raw = match fs::read(proc_path(pid, "cmdline")) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to read /proc/{pid}/cmdline"));
        }
    };

    if raw.is_empty() {
        return Ok(None);
    }

    let parts = raw
        .split(|byte| *byte == 0)
        .filter_map(|part| {
            let part = String::from_utf8_lossy(part);
            (!part.is_empty()).then_some(part.into_owned())
        })
        .collect::<Vec<_>>();

    Ok(Some(parts.join(" ")))
}

#[cfg(unix)]
fn proc_path(pid: u32, name: &str) -> PathBuf {
    Path::new("/proc").join(pid.to_string()).join(name)
}

#[cfg(unix)]
fn is_floppa_connect_cmdline(cmdline: &str) -> bool {
    let parts = cmdline.split_whitespace().collect::<Vec<_>>();
    let first = parts.first().copied().unwrap_or_default();
    let Some(first_file) = Path::new(first).file_name().and_then(|name| name.to_str()) else {
        return false;
    };

    matches!(first_file, "floppa-cli" | "floppa-cli-dev")
        && parts
            .windows(2)
            .any(|window| window[0] == "connect" || window[1] == "connect")
}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn parses_process_group_from_proc_stat() {
        let stat = "91265 (floppa-cli) S 91219 91265 91265 0 -1 4194560 1 0 0 0";
        let after_comm = stat.rsplit_once(')').unwrap().1;
        let fields = after_comm.split_whitespace().collect::<Vec<_>>();

        assert_eq!(fields[2].parse::<u32>().unwrap(), 91265);
    }

    #[test]
    fn detects_floppa_connect_cmdline() {
        assert!(is_floppa_connect_cmdline(
            "bin/floppa-cli connect --protocol amneziawg"
        ));
        assert!(is_floppa_connect_cmdline(
            "floppa-cli-dev --log-file log/floppa.log connect"
        ));
        assert!(!is_floppa_connect_cmdline("bin/floppa-cli stop"));
        assert!(!is_floppa_connect_cmdline("system/floppa-helper connect"));
    }

    #[test]
    fn joins_cmdline_parts_without_empty_segments() {
        let raw = OsString::from("floppa-cli\0connect\0--no-dns\0");
        let parts = raw
            .as_encoded_bytes()
            .split(|byte| *byte == 0)
            .filter_map(|part| {
                let part = String::from_utf8_lossy(part);
                (!part.is_empty()).then_some(part.into_owned())
            })
            .collect::<Vec<_>>();

        assert_eq!(parts.join(" "), "floppa-cli connect --no-dns");
    }
}
