use crate::paths;
use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ServiceScope {
    /// Install/manage a system service with `sudo systemctl`.
    System,
    /// Install/manage a user service with `systemctl --user`.
    User,
}

#[derive(Clone, Debug)]
pub struct ServiceInstallOptions {
    pub scope: ServiceScope,
    pub name: String,
    pub binary: PathBuf,
    pub protocol: String,
    pub interface: String,
    pub no_dns: bool,
    pub api_url: String,
    pub user: String,
    pub home: PathBuf,
    pub log_file: PathBuf,
}

#[derive(Clone, Debug)]
pub struct ServiceUninstallOptions {
    pub scope: ServiceScope,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
    Status,
    Enable,
    Disable,
}

#[derive(Clone, Debug)]
pub struct ServiceControlOptions {
    pub scope: ServiceScope,
    pub name: String,
    pub action: ServiceAction,
}

pub fn install(opts: &ServiceInstallOptions) -> Result<()> {
    validate_service_name(&opts.name)?;

    let unit = render_unit(opts)?;
    let unit_path = unit_path(opts.scope, &opts.name);

    match opts.scope {
        ServiceScope::System => {
            create_user_state_dir(opts)?;
            let temp_path =
                std::env::temp_dir().join(format!("{}.{}.tmp", opts.name, std::process::id()));
            fs::write(&temp_path, unit).with_context(|| {
                format!(
                    "Failed to write temporary unit file {}",
                    temp_path.display()
                )
            })?;

            let status = paths::command("sudo")
                .arg("install")
                .arg("-D")
                .arg("-m")
                .arg("0644")
                .arg(&temp_path)
                .arg(&unit_path)
                .status()
                .with_context(|| {
                    format!("Failed to run `sudo install` for {}", unit_path.display())
                })?;
            let _ = fs::remove_file(&temp_path);
            if !status.success() {
                bail!("Failed to install systemd unit to {}", unit_path.display());
            }
            daemon_reload(ServiceScope::System)?;
        }
        ServiceScope::User => {
            if let Some(parent) = unit_path.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "Failed to create systemd user directory {}",
                        parent.display()
                    )
                })?;
            }
            fs::write(&unit_path, unit)
                .with_context(|| format!("Failed to write unit file {}", unit_path.display()))?;
            daemon_reload(ServiceScope::User)?;
        }
    }

    println!("Installed {} at {}", opts.name, unit_path.display());
    Ok(())
}

pub fn uninstall(opts: &ServiceUninstallOptions) -> Result<()> {
    validate_service_name(&opts.name)?;
    let unit_path = unit_path(opts.scope, &opts.name);

    match opts.scope {
        ServiceScope::System => {
            let status = paths::command("sudo")
                .arg("rm")
                .arg("-f")
                .arg(&unit_path)
                .status()
                .with_context(|| format!("Failed to run `sudo rm -f {}`", unit_path.display()))?;
            if !status.success() {
                bail!("Failed to remove systemd unit {}", unit_path.display());
            }
            daemon_reload(ServiceScope::System)?;
        }
        ServiceScope::User => {
            if unit_path.exists() {
                fs::remove_file(&unit_path).with_context(|| {
                    format!("Failed to remove unit file {}", unit_path.display())
                })?;
            }
            daemon_reload(ServiceScope::User)?;
        }
    }

    println!("Removed {} from {}", opts.name, unit_path.display());
    Ok(())
}

pub fn control(opts: &ServiceControlOptions) -> Result<()> {
    validate_service_name(&opts.name)?;
    let action_arg = match opts.action {
        ServiceAction::Start => "start",
        ServiceAction::Stop => "stop",
        ServiceAction::Restart => "restart",
        ServiceAction::Enable => "enable",
        ServiceAction::Disable => "disable",
        ServiceAction::Status => "status",
    };

    if opts.action == ServiceAction::Status {
        let output = systemctl_output(opts.scope, false, ["--no-pager", "status", &opts.name])?;
        std::io::stdout().write_all(&output.stdout)?;
        if !output.stderr.is_empty() {
            std::io::stderr().write_all(&output.stderr)?;
        }
        return Ok(());
    }

    systemctl_success(opts.scope, [action_arg, &opts.name])
}

pub fn render_unit(opts: &ServiceInstallOptions) -> Result<String> {
    validate_service_name(&opts.name)?;
    validate_absolute_path("binary", &opts.binary)?;
    validate_absolute_path("home", &opts.home)?;
    validate_absolute_path("log file", &opts.log_file)?;

    if opts.scope == ServiceScope::System && opts.user.is_empty() {
        bail!("User must be set for system-scope service units");
    }
    if opts.interface.is_empty()
        || opts
            .interface
            .chars()
            .any(|c| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
    {
        bail!(
            "Interface name '{}' is invalid (ASCII alphanumeric, '-', '_' only)",
            opts.interface
        );
    }
    if !opts.api_url.starts_with("https://") && !opts.api_url.starts_with("http://") {
        bail!(
            "api_url must start with http:// or https://: {}",
            opts.api_url
        );
    }

    let mut exec_args = vec![
        opts.binary.clone(),
        PathBuf::from("connect"),
        PathBuf::from("--protocol"),
        PathBuf::from(&opts.protocol),
        PathBuf::from("--interface"),
        PathBuf::from(&opts.interface),
    ];
    // System-scoped services run as non-root with only CAP_NET_ADMIN/CAP_NET_RAW —
    // writing /etc/resolv.conf requires DAC_OVERRIDE which is absent by design.
    // Force --no-dns for system scope to prevent a crash loop on every start.
    let no_dns = opts.no_dns || opts.scope == ServiceScope::System;
    if no_dns {
        exec_args.push(PathBuf::from("--no-dns"));
    }
    if opts.api_url != DEFAULT_API_URL {
        exec_args.push(PathBuf::from("--api-url"));
        exec_args.push(PathBuf::from(&opts.api_url));
    }
    exec_args.push(PathBuf::from("--log-file"));
    exec_args.push(opts.log_file.clone());

    let exec_start = format!(
        "ExecStart={}",
        exec_args
            .iter()
            .map(|arg| quote_systemd_arg(arg.to_string_lossy().as_ref()))
            .collect::<Vec<_>>()
            .join(" ")
    );

    let mut lines = vec![
        "[Unit]".to_string(),
        "Description=Floppa VPN tunnel managed by floppa-cli".to_string(),
        "After=network-online.target".to_string(),
        "Wants=network-online.target".to_string(),
        String::new(),
        "[Service]".to_string(),
        "Type=simple".to_string(),
    ];

    if opts.scope == ServiceScope::System {
        lines.push(format!("User={}", opts.user));
        lines.push(format!("Environment=HOME={}", opts.home.display()));
        lines.push(format!("WorkingDirectory={}", opts.home.display()));
    }

    lines.extend([
        exec_start,
        "Restart=on-failure".to_string(),
        "RestartSec=5s".to_string(),
        "SuccessExitStatus=0 130 143".to_string(),
        "KillSignal=SIGTERM".to_string(),
        "TimeoutStopSec=30".to_string(),
        "NoNewPrivileges=true".to_string(),
        "CapabilityBoundingSet=CAP_NET_ADMIN CAP_NET_RAW".to_string(),
        "AmbientCapabilities=CAP_NET_ADMIN CAP_NET_RAW".to_string(),
        "StandardOutput=journal".to_string(),
        "StandardError=journal".to_string(),
        String::new(),
        "[Install]".to_string(),
    ]);

    let wanted_by = match opts.scope {
        ServiceScope::System => "multi-user.target",
        ServiceScope::User => "default.target",
    };
    lines.push(format!("WantedBy={wanted_by}"));
    lines.push(String::new());

    Ok(lines.join("\n"))
}

pub fn unit_path(scope: ServiceScope, name: &str) -> PathBuf {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"));
    unit_path_with_config_home(scope, name, config_home)
}

fn unit_path_with_config_home(scope: ServiceScope, name: &str, config_home: PathBuf) -> PathBuf {
    match scope {
        ServiceScope::System => Path::new("/etc/systemd/system").join(format!("{name}.service")),
        ServiceScope::User => config_home
            .join("systemd/user")
            .join(format!("{name}.service")),
    }
}

fn create_user_state_dir(opts: &ServiceInstallOptions) -> Result<()> {
    let state_dir = opts.log_file.parent().context("Log file has no parent")?;
    let status = paths::command("sudo")
        .arg("install")
        .arg("-d")
        .arg("-o")
        .arg(&opts.user)
        .arg("-g")
        .arg(&opts.user)
        .arg("-m")
        .arg("0755")
        .arg(state_dir)
        .status()
        .with_context(|| format!("Failed to create {}", state_dir.display()))?;
    if !status.success() {
        bail!("Failed to create {}", state_dir.display());
    }
    Ok(())
}

fn daemon_reload(scope: ServiceScope) -> Result<()> {
    systemctl_success(scope, ["daemon-reload"])
}

fn systemctl_success<I, S>(scope: ServiceScope, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut command = systemctl_command(scope, true);
    command.args(args);
    let output = command.output().context("Failed to run systemctl")?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("systemctl failed: {stderr}");
    }
}

fn systemctl_output<I, S>(
    scope: ServiceScope,
    privileged: bool,
    args: I,
) -> Result<std::process::Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut command = systemctl_command(scope, privileged);
    command.args(args);
    command.output().context("Failed to run systemctl")
}

fn systemctl_command(scope: ServiceScope, privileged: bool) -> std::process::Command {
    match (scope, privileged) {
        (ServiceScope::System, true) => {
            let mut command = paths::command("sudo");
            command.arg("systemctl");
            command
        }
        (ServiceScope::System, false) => paths::command("systemctl"),
        (ServiceScope::User, _) => {
            let mut command = paths::command("systemctl");
            command.arg("--user");
            command
        }
    }
}

fn validate_service_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("Service name cannot be empty");
    }
    if name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        Ok(())
    } else {
        bail!("Service name may contain only ASCII letters, digits, `-`, `_`, and `.`")
    }
}

fn validate_absolute_path(label: &str, path: &Path) -> Result<()> {
    if path.is_absolute() {
        Ok(())
    } else {
        bail!("{label} must be an absolute path: {}", path.display())
    }
}

fn quote_systemd_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }

    let needs_quotes = arg
        .chars()
        .any(|ch| ch.is_whitespace() || matches!(ch, '\'' | '"' | '\\' | '$' | '`'));
    if !needs_quotes {
        return arg.to_string();
    }

    format!("'{}'", arg.replace('\\', "\\\\").replace('\'', "\\'"))
}

const DEFAULT_API_URL: &str = "https://floppa.okhsunrog.dev/api";

#[cfg(test)]
mod tests {
    use super::*;

    fn service_options(scope: ServiceScope) -> ServiceInstallOptions {
        ServiceInstallOptions {
            scope,
            name: "floppa-cli".to_string(),
            binary: PathBuf::from("/srv/floppa-test/bin/floppa-cli"),
            protocol: "amneziawg".to_string(),
            interface: "floppa0".to_string(),
            no_dns: true,
            api_url: DEFAULT_API_URL.to_string(),
            user: "test-user".to_string(),
            home: PathBuf::from("/srv/floppa-test/home/test-user"),
            log_file: PathBuf::from(
                "/srv/floppa-test/home/test-user/.local/state/floppa-cli/floppa-cli.log",
            ),
        }
    }

    #[test]
    fn renders_system_unit_for_connect() {
        let unit = render_unit(&service_options(ServiceScope::System)).unwrap();
        let exec_start = unit
            .lines()
            .find(|line| line.starts_with("ExecStart="))
            .expect("ExecStart line");

        assert!(unit.contains("User=test-user"));
        assert!(unit.contains("Environment=HOME=/srv/floppa-test/home/test-user"));
        assert!(unit.contains("WantedBy=multi-user.target"));
        assert_eq!(
            exec_start,
            "ExecStart=/srv/floppa-test/bin/floppa-cli connect --protocol amneziawg --interface floppa0 --no-dns --log-file /srv/floppa-test/home/test-user/.local/state/floppa-cli/floppa-cli.log"
        );
        assert!(unit.contains("AmbientCapabilities=CAP_NET_ADMIN CAP_NET_RAW"));
    }

    #[test]
    fn renders_user_unit_for_default_target() {
        let unit = render_unit(&service_options(ServiceScope::User)).unwrap();
        let exec_start = unit
            .lines()
            .find(|line| line.starts_with("ExecStart="))
            .expect("ExecStart line");

        assert!(!unit.contains("User="));
        assert!(unit.contains("WantedBy=default.target"));
        assert_eq!(
            exec_start,
            "ExecStart=/srv/floppa-test/bin/floppa-cli connect --protocol amneziawg --interface floppa0 --no-dns --log-file /srv/floppa-test/home/test-user/.local/state/floppa-cli/floppa-cli.log"
        );
    }

    #[test]
    fn includes_api_url_when_non_default() {
        let mut opts = service_options(ServiceScope::System);
        opts.api_url = "https://example.invalid/api".to_string();

        let unit = render_unit(&opts).unwrap();

        assert!(unit.contains("--api-url https://example.invalid/api"));
    }

    #[test]
    fn quotes_exec_start_arguments_with_spaces() {
        let mut opts = service_options(ServiceScope::System);
        opts.binary = PathBuf::from("/srv/floppa-test/bin/floppa cli");
        opts.log_file =
            PathBuf::from("/srv/floppa-test/home/test-user/.local/state/floppa cli/floppa-cli.log");

        let unit = render_unit(&opts).unwrap();
        let exec_start = unit
            .lines()
            .find(|line| line.starts_with("ExecStart="))
            .expect("ExecStart line");

        assert_eq!(
            exec_start,
            "ExecStart='/srv/floppa-test/bin/floppa cli' connect --protocol amneziawg --interface floppa0 --no-dns --log-file '/srv/floppa-test/home/test-user/.local/state/floppa cli/floppa-cli.log'"
        );
    }

    #[test]
    fn rejects_invalid_service_names() {
        let mut opts = service_options(ServiceScope::System);
        opts.name = "../floppa-cli".to_string();
        assert!(render_unit(&opts).is_err());

        let mut opts = service_options(ServiceScope::System);
        opts.name = "".to_string();
        assert!(render_unit(&opts).is_err());
    }

    #[test]
    fn rejects_relative_paths_in_unit_rendering() {
        let mut opts = service_options(ServiceScope::System);
        opts.binary = PathBuf::from("floppa-cli");
        assert!(render_unit(&opts).is_err());

        let mut opts = service_options(ServiceScope::System);
        opts.home = PathBuf::from("home/test-user");
        assert!(render_unit(&opts).is_err());

        let mut opts = service_options(ServiceScope::System);
        opts.log_file = PathBuf::from("floppa-cli.log");
        assert!(render_unit(&opts).is_err());
    }

    #[test]
    fn omits_no_dns_flag_when_dns_enabled_user_scope() {
        let mut opts = service_options(ServiceScope::User);
        opts.no_dns = false;

        let unit = render_unit(&opts).unwrap();
        assert!(!unit.contains("--no-dns"));
    }

    #[test]
    fn system_scope_always_adds_no_dns() {
        let mut opts = service_options(ServiceScope::System);
        opts.no_dns = false;

        let unit = render_unit(&opts).unwrap();
        assert!(unit.contains("--no-dns"));
    }

    #[test]
    fn omits_api_url_when_default() {
        let unit = render_unit(&service_options(ServiceScope::System)).unwrap();
        assert!(!unit.contains("--api-url"));
    }

    #[test]
    fn accepts_valid_service_names() {
        for name in ["floppa-cli", "my.service_name", "abc123", "a-b_c.d"] {
            let mut opts = service_options(ServiceScope::System);
            opts.name = name.to_string();
            assert!(render_unit(&opts).is_ok(), "name {name:?} should be valid");
        }
    }

    #[test]
    fn unit_path_system_is_etc_systemd() {
        let path = unit_path(ServiceScope::System, "floppa-cli");
        assert_eq!(
            path,
            PathBuf::from("/etc/systemd/system/floppa-cli.service")
        );
    }

    #[test]
    fn unit_path_user_respects_xdg_config_home() {
        let path = unit_path_with_config_home(
            ServiceScope::User,
            "floppa-cli",
            PathBuf::from("/tmp/test-config"),
        );
        assert_eq!(
            path,
            PathBuf::from("/tmp/test-config/systemd/user/floppa-cli.service")
        );
    }

    #[test]
    fn quote_systemd_arg_empty_string() {
        assert_eq!(quote_systemd_arg(""), "''");
    }

    #[test]
    fn quote_systemd_arg_no_special_chars() {
        assert_eq!(
            quote_systemd_arg("/usr/bin/floppa-cli"),
            "/usr/bin/floppa-cli"
        );
    }

    #[test]
    fn quote_systemd_arg_with_space() {
        assert_eq!(
            quote_systemd_arg("/path/to my/binary"),
            "'/path/to my/binary'"
        );
    }

    #[test]
    fn quote_systemd_arg_with_single_quote() {
        assert_eq!(quote_systemd_arg("it's"), "'it\\'s'");
    }

    #[test]
    fn quote_systemd_arg_with_backslash() {
        assert_eq!(quote_systemd_arg("foo\\bar"), "'foo\\\\bar'");
    }

    #[test]
    fn quote_systemd_arg_with_dollar() {
        assert_eq!(quote_systemd_arg("$HOME"), "'$HOME'");
    }
}
