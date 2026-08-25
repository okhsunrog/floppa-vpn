//! Rollback of the host-side changes a connection makes (routes, DNS).
//!
//! `Rollback::run` is the normal path. If it never runs — a panic, or an error propagated with
//! `?` after the routes are in place — `Drop` runs the same steps synchronously, so the host is
//! not left with a stale endpoint route or a VPN `/etc/resolv.conf`. Every step is `std`-only
//! (`ip` subprocesses and file writes), which is what makes it safe in `Drop`.

use anyhow::Result;

use crate::dns::DnsBackup;
use crate::net::{self, AppliedNetworking};

pub struct Rollback {
    networking: Option<AppliedNetworking>,
    dns: Option<DnsBackup>,
}

impl Rollback {
    pub fn new(networking: AppliedNetworking) -> Self {
        Self {
            networking: Some(networking),
            dns: None,
        }
    }

    /// Record what `dns::apply` changed, so it is restored with the routes.
    pub fn set_dns(&mut self, backup: DnsBackup) {
        self.dns = Some(backup);
    }

    /// Restore DNS and tear down the routes. Each step runs even if the previous one failed;
    /// the errors are collected. Afterwards the guard is disarmed: `Drop` does nothing more.
    pub fn run(&mut self) -> Result<()> {
        let mut errors = Vec::new();
        if let Some(dns) = self.dns.take()
            && let Err(e) = dns.restore()
        {
            errors.push(e);
        }
        if let Some(mut networking) = self.networking.take()
            && let Err(e) = networking.teardown()
        {
            errors.push(e);
        }
        net::collect_errors(errors)
    }

    fn is_armed(&self) -> bool {
        self.dns.is_some() || self.networking.is_some()
    }
}

impl Drop for Rollback {
    fn drop(&mut self) {
        if !self.is_armed() {
            return;
        }
        eprintln!("Rolling back network changes...");
        if let Err(e) = self.run() {
            eprintln!("Rollback incomplete: {e:#}");
        }
    }
}
