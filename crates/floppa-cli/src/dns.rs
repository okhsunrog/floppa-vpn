use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

use crate::tunnel::WgConfig;

const RESOLV_CONF: &str = "/etc/resolv.conf";
const RESOLV_BACKUP: &str = "/etc/resolv.conf.floppa-backup";

pub fn set_dns(config: &WgConfig) -> Result<()> {
    let servers = config.dns_servers();
    if servers.is_empty() {
        return Ok(());
    }

    // Backup current resolv.conf
    if Path::new(RESOLV_CONF).exists() && !Path::new(RESOLV_BACKUP).exists() {
        fs::copy(RESOLV_CONF, RESOLV_BACKUP)
            .context("Failed to backup /etc/resolv.conf")?;
    }

    let content: String = servers
        .iter()
        .map(|s| format!("nameserver {s}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";

    fs::write(RESOLV_CONF, content).context("Failed to write /etc/resolv.conf")?;
    eprintln!("DNS: {}", servers.join(", "));

    Ok(())
}

pub fn restore_dns() -> Result<()> {
    if Path::new(RESOLV_BACKUP).exists() {
        fs::copy(RESOLV_BACKUP, RESOLV_CONF)
            .context("Failed to restore /etc/resolv.conf")?;
        fs::remove_file(RESOLV_BACKUP).ok();
        eprintln!("DNS restored.");
    }
    Ok(())
}
