use anyhow::{Context, Result};
use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

const LOCAL_BIN: &str = ".local/bin";

/// Returns `$XDG_CONFIG_HOME/floppa-cli` or `~/.config/floppa-cli`, creating it if needed.
pub fn floppa_config_dir() -> Result<PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(dirs::config_dir)
        .context("Cannot determine config directory")?
        .join("floppa-cli");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.env("PATH", configured_path());
    cmd
}

pub fn configured_path() -> OsString {
    let current_exe = env::current_exe().ok();
    let home = env::var_os("HOME");
    let path = env::var_os("PATH");

    configured_path_from(
        current_exe.as_deref(),
        home.as_deref().map(Path::new),
        path.as_deref(),
    )
}

fn configured_path_from(
    current_exe: Option<&Path>,
    home: Option<&Path>,
    path: Option<&OsStr>,
) -> OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(parent) = current_exe.and_then(Path::parent) {
        push_unique(&mut dirs, parent.to_path_buf());
    }

    if let Some(home) = home {
        push_unique(&mut dirs, PathBuf::from(home).join(LOCAL_BIN));
    }

    if let Some(path) = path.filter(|path| !path.is_empty()) {
        for dir in env::split_paths(path) {
            push_unique(&mut dirs, dir);
        }
    } else {
        for dir in env::split_paths(&default_system_path()) {
            push_unique(&mut dirs, dir);
        }
    }

    env::join_paths(&dirs).unwrap_or_else(|_| path.unwrap_or_default().to_os_string())
}

fn default_system_path() -> OsString {
    if cfg!(windows) {
        r"C:\Windows\System32;C:\Windows".into()
    } else {
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into()
    }
}

fn push_unique(dirs: &mut Vec<PathBuf>, dir: PathBuf) {
    if !dirs.iter().any(|existing| existing == &dir) {
        dirs.push(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn paths(path: &OsStr) -> Vec<String> {
        env::split_paths(path)
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn prepends_exe_parent_and_local_bin_without_duplicates() {
        let path = configured_path_from(
            Some(Path::new("bin/floppa-cli")),
            Some(Path::new("home")),
            Some(OsStr::new("existing:/usr/bin:existing")),
        );

        let dirs = paths(&path);

        assert_eq!(dirs[0], "bin");
        assert_eq!(dirs[1], "home/.local/bin");
        assert_eq!(dirs[2], "existing");
        assert_eq!(dirs[3], "/usr/bin");
        assert_eq!(dirs.len(), 4);
    }

    #[test]
    fn handles_empty_path() {
        let path = configured_path_from(
            Some(Path::new("bin/floppa-cli")),
            Some(Path::new("home")),
            Some(OsStr::new("")),
        );

        let dirs = paths(&path);

        assert_eq!(dirs[0], "bin");
        assert_eq!(dirs[1], "home/.local/bin");
        assert!(dirs.contains(&"/usr/bin".to_string()));
        assert!(!dirs.contains(&"".to_string()));
    }

    #[test]
    fn keeps_existing_path_when_join_fails() {
        let invalid = if cfg!(windows) {
            OsStr::new(r"C:\bad\dir;bad\dir")
        } else {
            OsStr::new("/good:/bad\0dir")
        };

        let path = configured_path_from(None, None, Some(invalid));

        assert_eq!(path, invalid);
    }
}
