//! How this installation puts a tunnel on the machine.
//!
//! Not policy and not state: three facts about the program the actor is running inside, fixed for
//! its lifetime. They exist because the app and the command-line client are the same tunnel in
//! two different deployments, and the differences between them are exactly these.

use crate::protocol::InterfaceName;

/// Where the actor's configs come from, and whether changes are written back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfigSource {
    /// Whatever this installation has persisted, with changes written back — the OS keyring where
    /// there is one, a `0600` file otherwise. What an app wants: a config imported once is there
    /// on the next launch.
    #[default]
    Persisted,
    /// Nothing to start from, and nothing written anywhere.
    ///
    /// What a one-shot command wants. A CLI run is handed its config on the command line or
    /// fetches a fresh one, and must not leave a copy behind — least of all in the keyring of
    /// whoever it is running as, which under `sudo` is root.
    Ephemeral,
}

/// The fixed facts about the program the actor runs inside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deployment {
    /// The name the tunnel interface gets.
    pub iface: InterfaceName,
    /// Whether the tunnel's DNS servers are applied to the system.
    ///
    /// False leaves resolution exactly as it was. Not a way to say "this config has no DNS" —
    /// that is the config's business, and it says so by naming no servers — but a way for a
    /// caller that knows the machine's resolution must not be touched to say so: a container in
    /// a test harness, or a user who has arranged their own.
    pub manage_dns: bool,
    pub configs: ConfigSource,
}

impl Default for Deployment {
    fn default() -> Self {
        Self {
            iface: InterfaceName::default(),
            manage_dns: true,
            configs: ConfigSource::default(),
        }
    }
}
