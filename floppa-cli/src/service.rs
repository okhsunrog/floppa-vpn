use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use std::collections::HashMap;
use std::path::PathBuf;

/// Systemd service management commands
#[derive(Subcommand, Debug, Clone)]
pub enum ServiceCommand {
    /// Install a systemd unit for `floppa-cli connect`
    Install {
        /// Service scope: system (`sudo systemctl`) or user (`systemctl --user`)
        #[arg(long, value_enum, default_value_t = ServiceScope::System)]
        scope: ServiceScope,
        /// Service name without `.service`
        #[arg(long, default_value = "floppa-cli")]
        name: String,
        /// Absolute path to the floppa-cli binary
        #[arg(long, default_value = "$(which floppa-cli)")]
        binary_path: Option<PathBuf>,
        /// Environment variables for the service (KEY=VALUE)
        #[arg(long)]
        env: Vec<String>,
        /// Arguments to pass to the CLI
        #[arg(long)]
        args: Vec<String>,
        /// Service log file path (optional)
        #[arg(long)]
        service_log_file: Option<PathBuf>,
    },
    /// Start a running systemd service
    Start {
        /// Service name without `.service`
        #[arg(long, default_value = "floppa-cli")]
        name: String,
        /// Service scope
        #[arg(long, value_enum, default_value_t = ServiceScope::System)]
        scope: ServiceScope,
    },
    /// Stop a running systemd service
    Stop {
        /// Service name without `.service`
        #[arg(long, default_value = "floppa-cli")]
        name: String,
        /// Service scope
        #[arg(long, value_enum, default_value_t = ServiceScope::System)]
        scope: ServiceScope,
        /// Target a specific floppa-cli connect PID when multiple are running
        #[arg(long)]
        pid: Option<u32>,
        /// Send SIGKILL if graceful SIGTERM stop times out
        #[arg(long)]
        force: bool,
    },
    /// Restart a systemd service
    Restart {
        /// Service name without `.service`
        #[arg(long, default_value = "floppa-cli")]
        name: String,
        /// Service scope
        #[arg(long, value_enum, default_value_t = ServiceScope::System)]
        scope: ServiceScope,
    },
    /// Remove an installed systemd unit
    Uninstall {
        /// Service name without `.service`
        #[arg(long, default_value = "floppa-cli")]
        name: String,
        /// Service scope
        #[arg(long, value_enum, default_value_t = ServiceScope::System)]
        scope: ServiceScope,
    },
}

/// Service scope: system (`sudo systemctl`) or user (`systemctl --user`)
#[derive(Debug, Clone, PartialEq, clap::ValueEnum)]
pub enum ServiceScope {
    System,
    User,
}

/// Handle systemd service command execution
pub async fn handle_service_command(command: ServiceCommand) -> Result<()> {
    match command {
        ServiceCommand::Install {
            scope,
            name,
            binary_path,
            env,
            args,
            service_log_file,
        } => {
            install_service(scope, name, binary_path, env, args, service_log_file).await?
        }
        ServiceCommand::Start { scope, name, .. } => {
            start_service(scope, name).await?
        }
        ServiceCommand::Stop {
            scope,
            name,
            pid,
            force,
        } => {
            stop_service(scope, name, pid, force).await?
        }
        ServiceCommand::Restart { scope, name, .. } => {
            restart_service(scope, name).await?
        }
        ServiceCommand::Uninstall { scope, name, .. } => {
            uninstall_service(scope, name).await?
        }
    }
    Ok(())
}

/// Install systemd service unit
async fn install_service(
    scope: ServiceScope,
    name: String,
    binary_path: Option<PathBuf>,
    env: Vec<String>,
    args: Vec<String>,
    service_log_file: Option<PathBuf>,
) -> Result<()> {
    let home = std::env::var("HOME").context("HOME environment variable not set")?;
    let user = std::env::var("USER").unwrap_or_else(|_| "root".to_string());

    let user = if user == "root" {
        std::env::var("SUDO_USER")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or(user)
    } else {
        user
    };

    let binary_path = binary_path.unwrap_or_else(|| PathBuf::from("/home/$USER/.local/bin/floppa-cli"));

    let log_file = service_log_file.unwrap_or_else(|| {
        PathBuf::from(&home)
            .join(".local")
            .join("state")
            .join("floppa-cli")
            .join(format!("{}.log", name))
    });

    println!("Installing {} service for user '{}'", scope, user);
    println!("Service name: {}", name);
    println!("Binary path: {:?}", binary_path);
    println!("Log file: {:?}", log_file);

    match scope {
        ServiceScope::System => {
            install_system_service(user, &name, &binary_path, &env, &args, &log_file).await?
        }
        ServiceScope::User => {
            install_user_service(user, &name, &binary_path, &env, &args, &log_file).await?
        }
    }

    println!("Service installed successfully!");
    Ok(())
}

/// Install system-wide systemd service
async fn install_system_service(
    user: String,
    name: &str,
    binary_path: &PathBuf,
    env: &[String],
    args: &[String],
    log_file: &PathBuf,
) -> Result<()> {
    let sudo_user = if user == "root" {
        std::env::var("SUDO_USER").unwrap_or_else(|_| "$SUDO_USER".to_string())
    } else {
        user.clone()
    };

    let home = if user == "root" {
        format!("/home/{$SUDO_USER}")
    } else {
        format!("/home/{}", user)
    };

    let unit_content = generate_systemd_unit(
        name,
        binary_path,
        env,
        args,
        log_file,
        true, // system scope
    );

    let unit_path = format!("/etc/systemd/system/{}.service", name);

    println!("Writing systemd unit to: {}", unit_path);

    let cmd = if user != "root" {
        format!("sudo install -o {} -g {} -m 644 /dev/stdin {}",
                sudo_user, sudo_user, unit_path)
    } else {
        format!("install -o {} -g {} -m 644 /dev/stdin {}",
                user, user, unit_path)
    };

    let install_result = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("cat > {} << 'EOF'
{}
EOF", unit_path, unit_content))
        .output()
        .context("Failed to install systemd unit")?;

    if !install_result.status.success() {
        bail!("Failed to install systemd unit: {}",
              String::from_utf8_lossy(&install_result.stderr));
    }

    let reload_cmd = if user != "root" {
        format!("sudo systemctl --user daemon-reload")
    } else {
        format!("systemctl daemon-reload")
    };

    let reload_result = std::process::Command::new("sh")
        .arg("-c")
        .arg(reload_cmd)
        .output()
        .context("Failed to reload systemd daemon")?;

    if !reload_result.status.success() {
        bail!("Failed to reload systemd daemon: {}",
              String::from_utf8_lossy(&reload_result.stderr));
    }

    let enable_cmd = if user != "root" {
        format!("sudo systemctl --user enable {}.service", name)
    } else {
        format!("systemctl enable {}.service", name)
    };

    let enable_result = std::process::Command::new("sh")
        .arg("-c")
        .arg(enable_cmd)
        .output()
        .context("Failed to enable systemd service")?;

    if !enable_result.status.success() {
        bail!("Failed to enable systemd service: {}",
              String::from_utf8_lossy(&enable_result.stderr));
    }

    println!("Service {}.service enabled successfully", name);
    Ok(())
}

/// Install user-level systemd service
async fn install_user_service(
    user: String,
    name: &str,
    binary_path: &PathBuf,
    env: &[String],
    args: &[String],
    log_file: &PathBuf,
) -> Result<()> {
    let home = format!("/home/{}", user);
    let user_unit_dir = format!("{}/.config/systemd/user", home);

    std::fs::create_dir_all(&user_unit_dir).context(format!("Failed to create user unit directory: {}", user_unit_dir))?;

    let unit_content = generate_systemd_unit(
        name,
        binary_path,
        env,
        args,
        log_file,
        false, // user scope
    );

    let unit_path = format!("{}/{}.service", user_unit_dir, name);

    println!("Writing systemd unit to: {}", unit_path);

    let write_result = std::fs::write(&unit_path, unit_content).context(format!("Failed to write systemd unit to {}", unit_path));

    if !write_result.is_ok() {
        bail!("Failed to write systemd unit to {}: {}", unit_path, write_result.unwrap_err());
    }

    let reload_result = std::process::Command::new("systemctl")
        .arg("--user")
        .arg("daemon-reload")
        .output()
        .context("Failed to reload user systemd daemon")?;

    if !reload_result.status.success() {
        bail!("Failed to reload user systemd daemon: {}",
              String::from_utf8_lossy(&reload_result.stderr));
    }

    let enable_result = std::process::Command::new("systemctl")
        .arg("--user")
        .arg("enable")
        .arg(format!("{}.service", name))
        .output()
        .context("Failed to enable user systemd service")?;

    if !enable_result.status.success() {
        bail!("Failed to enable user systemd service: {}",
              String::from_utf8_lossy(&enable_result.stderr));
    }

    println!("User service {}.service enabled successfully", name);
    Ok(())
}

/// Generate systemd unit file content
fn generate_systemd_unit(
    name: &str,
    binary_path: &PathBuf,
    env: &[String],
    args: &[String],
    log_file: &PathBuf,
    system_scope: bool,
) -> String {
    let user = if system_scope { "root" } else { "user" };

    let mut env_section = String::new();
    for env_var in env {
        let parts: Vec<&str> = env_var.splitn(2, '=').collect();
        if parts.len() == 2 {
            env_section.push_str(&format!("Environment={}={}\n", user, parts[0], parts[1]));
        }
    }

    let mut args_section = String::new();
    for arg in args {
        args_section.push_str(&format!("    {}", arg));
    }

    format!(
        "[Unit]\n" +
        "Description=Floppa VPN CLI Service ({})\n" +
        "After=network-online.target\n" +
        "Wants=network-online.target\n" +
        "\n" +
        "[Service]\n" +
        "Type=simple\n" +
        "User={}\n" +
        "Group={}\n" +
        "Environment=HOME=/{}\n" +
        "{}" +
        "ExecStart={} {}\n" +
        "Restart=on-failure\n" +
        "RestartSec=10\n" +
        "StandardOutput=append:{}\n" +
        "StandardError=append:{}\n" +
        "\n" +
        "[Install]\n" +
        "WantedBy={}",
        name,
        user,
        user,
        user,
        env_section,
        binary_path.to_string_lossy(),
        args_section,
        log_file.to_string_lossy(),
        log_file.to_string_lossy(),
        if system_scope { "multi-user.target" } else { "autostart.target" }
    )
}

/// Start systemd service
async fn start_service(scope: ServiceScope, name: String) -> Result<()> {
    match scope {
        ServiceScope::System => {
            let cmd = format!("sudo systemctl start {}.service", name);
            let result = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .output()
                .context("Failed to start systemd service")?;

            if !result.status.success() {
                bail!("Failed to start systemd service: {}",
                      String::from_utf8_lossy(&result.stderr));
            }

            println!("Service {}.service started successfully", name);
        }
        ServiceScope::User => {
            let cmd = format!("systemctl --user start {}.service", name);
            let result = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .output()
                .context("Failed to start user systemd service")?;

            if !result.status.success() {
                bail!("Failed to start user systemd service: {}",
                      String::from_utf8_lossy(&result.stderr));
            }

            println!("User service {}.service started successfully", name);
        }
    }

    Ok(())
}

/// Stop systemd service
async fn stop_service(
    scope: ServiceScope,
    name: String,
    pid: Option<u32>,
    force: bool,
) -> Result<()> {
    match scope {
        ServiceScope::System => {
            if let Some(target_pid) = pid {
                let stop_cmd = if force {
                    format!("sudo kill -9 {}", target_pid)
                } else {
                    format!("sudo kill -TERM {}", target_pid)
                };

                let result = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(stop_cmd)
                    .output()
                    .context("Failed to stop service by PID")?;

                if !result.status.success() {
                    println!("Warning: Failed to stop service by PID: {}",
                             String::from_utf8_lossy(&result.stderr));
                }

                return Ok(());
            }

            let stop_cmd = if force {
                format!("sudo systemctl stop {}.service", name)
            } else {
                format!("sudo systemctl stop {}.service", name)
            };

            let result = std::process::Command::new("sh")
                .arg("-c")
                .arg(stop_cmd)
                .output()
                .context("Failed to stop systemd service")?;

            if !result.status.success() {
                bail!("Failed to stop systemd service: {}",
                      String::from_utf8_lossy(&result.stderr));
            }

            println!("Service {}.service stopped successfully", name);
        }
        ServiceScope::User => {
            let stop_cmd = if force {
                format!("systemctl --user stop {}.service", name)
            } else {
                format!("systemctl --user stop {}.service", name)
            };

            let result = std::process::Command::new("sh")
                .arg("-c")
                .arg(stop_cmd)
                .output()
                .context("Failed to stop user systemd service")?;

            if !result.status.success() {
                bail!("Failed to stop user systemd service: {}",
                      String::from_utf8_lossy(&result.stderr));
            }

            println!("User service {}.service stopped successfully", name);
        }
    }

    Ok(())
}

/// Restart systemd service
async fn restart_service(scope: ServiceScope, name: String) -> Result<()> {
    match scope {
        ServiceScope::System => {
            let result = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("sudo systemctl restart {}.service", name))
                .output()
                .context("Failed to restart systemd service")?;

            if !result.status.success() {
                bail!("Failed to restart systemd service: {}",
                      String::from_utf8_lossy(&result.stderr));
            }

            println!("Service {}.service restarted successfully", name);
        }
        ServiceScope::User => {
            let result = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("systemctl --user restart {}.service", name))
                .output()
                .context("Failed to restart user systemd service")?;

            if !result.status.success() {
                bail!("Failed to restart user systemd service: {}",
                      String::from_utf8_lossy(&result.stderr));
            }

            println!("User service {}.service restarted successfully", name);
        }
    }

    Ok(())
}

/// Uninstall systemd service
async fn uninstall_service(scope: ServiceScope, name: String) -> Result<()> {
    match scope {
        ServiceScope::System => {
            let stop_cmd = format!("sudo systemctl stop {}.service", name);
            let result = std::process::Command::new("sh")
                .arg("-c")
                .arg(stop_cmd)
                .output()
                .context("Failed to stop systemd service before uninstall")?;

            if !result.status.success() {
                println!("Warning: Failed to stop service before uninstall: {}",
                         String::from_utf8_lossy(&result.stderr));
            }

            let disable_cmd = format!("sudo systemctl disable {}.service", name);
            let disable_result = std::process::Command::new("sh")
                .arg("-c")
                .arg(disable_cmd)
                .output()
                .context("Failed to disable systemd service")?;

            if !disable_result.status.success() {
                bail!("Failed to disable systemd service: {}",
                      String::from_utf8_lossy(&disable_result.stderr));
            }

            let unit_path = format!("/etc/systemd/system/{}.service", name);
            let remove_result = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("sudo rm -f {}", unit_path))
                .output()
                .context("Failed to remove systemd unit file")?;

            if !remove_result.status.success() {
                bail!("Failed to remove systemd unit file: {}",
                      String::from_utf8_lossy(&remove_result.stderr));
            }

            let reload_result = std::process::Command::new("sh")
                .arg("-c")
                .arg("sudo systemctl daemon-reload")
                .output()
                .context("Failed to reload systemd daemon")?;

            if !reload_result.status.success() {
                bail!("Failed to reload systemd daemon: {}",
                      String::from_utf8_lossy(&reload_result.stderr));
            }

            println!("System service {}.service uninstalled successfully", name);
        }
        ServiceScope::User => {
            let stop_cmd = format!("systemctl --user stop {}.service", name);
            let result = std::process::Command::new("sh")
                .arg("-c")
                .arg(stop_cmd)
                .output()
                .context("Failed to stop user systemd service before uninstall")?;

            if !result.status.success() {
                println!("Warning: Failed to stop user service before uninstall: {}",
                         String::from_utf8_lossy(&result.stderr));
            }

            let disable_cmd = format!("systemctl --user disable {}.service", name);
            let disable_result = std::process::Command::new("sh")
                .arg("-c")
                .arg(disable_cmd)
                .output()
                .context("Failed to disable user systemd service")?;

            if !disable_result.status.success() {
                bail!("Failed to disable user systemd service: {}",
                      String::from_utf8_lossy(&disable_result.stderr));
            }

            let home = std::env::var("HOME").context("HOME environment variable not set")?;
            let user_unit_dir = format!("{}/.config/systemd/user", home);
            let unit_path = format!("{}/{}.service", user_unit_dir, name);

            let remove_result = std::fs::remove_file(&unit_path).context(format!("Failed to remove user systemd unit file: {}", unit_path));

            if remove_result.is_err() {
                println!("Warning: Failed to remove user systemd unit file: {}", remove_result.unwrap_err());
            }

            let reload_result = std::process::Command::new("systemctl")
                .arg("--user")
                .arg("daemon-reload")
                .output()
                .context("Failed to reload user systemd daemon")?;

            if !reload_result.status.success() {
                bail!("Failed to reload user systemd daemon: {}",
                      String::from_utf8_lossy(&reload_result.stderr));
            }

            println!("User service {}.service uninstalled successfully", name);
        }
    }

    Ok(())
}