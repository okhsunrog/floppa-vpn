//! Which of the three ways of running a tunnel this client is in.
//!
//! A machine can hold the tunnel in one of three places, and a client has to pick one before it
//! does anything: the **system service**, an **unprivileged actor in this process** that raises
//! polkit for each change it makes, or an actor in this process that is **already root**. The
//! first is the one to prefer where it exists; the other two are what every other platform has,
//! and what a tarball or an AppImage has on Linux.
//!
//! # Decided once
//!
//! Whatever this answers, the caller keeps for its whole run. Not because re-checking is
//! expensive, but because the two modes must not both be live: a second actor would take the
//! interface the first is using and lay its own routes and DNS over them, holding a rollback
//! journal that knows nothing about what the other applied. Deciding once is what makes "one
//! actor owns the machine's network" a property rather than a hope. (The privileged helper
//! refuses the overlap too, as a floor under this — see `ensure-tun`.)
//!
//! # Being refused is not the same as finding nothing
//!
//! The distinction this module exists for. A socket that will not open because the user is not in
//! the group is a *service that is there*, and falling back to the in-process path would work —
//! polkit would ask for a password and the tunnel would come up — which is precisely the problem:
//! it would work slightly worse, forever, for a reason nobody would ever be shown. Every connect
//! would raise a prompt, the tunnel would still die with the program, and the answer ("you are not
//! in the `floppa` group") would never be said out loud.

use crate::rpc::{PROTOCOL_VERSION, STATE_POLL_DEADLINE, VpnRpcClient};
use std::path::Path;
use std::time::Duration;
use tokio_util::codec::length_delimited::LengthDelimitedCodec;
use tracing::debug;

/// How long to wait for a service to say what it is.
///
/// It looks generous for a call over a local socket, and it is not. Under socket activation the
/// connection succeeds *immediately* — the kernel queues it against a socket systemd is holding —
/// and only then does systemd fork and exec the service, which has to read its config store off
/// disk and start an actor before it can answer anything. So this is not the latency of a call, it
/// is the latency of a cold start, and shrinking it to what a warm service needs would make a
/// first connect report "no service" on a slow machine.
const PROBE_DEADLINE: Duration = Duration::from_secs(5);

/// What was found at the socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceAccess {
    /// A service is there, speaking this build's protocol. Drive it.
    Available,
    /// Nothing is listening. Run the tunnel in this process.
    ///
    /// Covers both "no socket file" and "a socket file left behind by a process that is gone",
    /// which is why this is decided by connecting rather than by looking: a stale socket refuses
    /// connections while still very much existing.
    Absent,
    /// A service is there and this user may not talk to it.
    Forbidden { detail: String },
    /// A service is there and speaks a protocol this build does not.
    ///
    /// Almost always an upgrade that replaced the binaries while the old service kept running.
    /// Distinct from [`Absent`](Self::Absent) because the answer is "restart the service", and
    /// quietly running a second actor beside a live one is the one thing that must not happen.
    WrongVersion { theirs: u32 },
}

impl ServiceAccess {
    /// What to tell a person who cannot be served, in their terms rather than the socket's.
    ///
    /// `None` for the two answers that are not a problem.
    pub fn explain(&self) -> Option<String> {
        match self {
            Self::Available | Self::Absent => None,
            Self::Forbidden { detail } => Some(format!(
                "the tunnel service is running, but this user may not use it ({detail}).\n\
                 Add yourself to the `floppa` group and log in again, or run this with sudo."
            )),
            Self::WrongVersion { theirs } => Some(format!(
                "the tunnel service speaks protocol {theirs} and this build speaks \
                 {PROTOCOL_VERSION} — it is still running the binary from before an update.\n\
                 Restart it with `systemctl restart floppa-vpn.service`."
            )),
        }
    }
}

/// The socket the system service listens on.
///
/// Linux-only, like the service. Android compiles the rest of this module — its UI probes a socket
/// too — but reaches `:vpn` at a path the app resolves, not a system one, so the names this needs
/// are spelled out here rather than imported and left unused there.
#[cfg(target_os = "linux")]
pub fn system_socket() -> std::path::PathBuf {
    Path::new(crate::rpc::SYSTEM_SOCKET_DIR).join(crate::rpc::SOCKET_NAME)
}

/// Ask the socket what is behind it.
///
/// Connects and makes one call, rather than stopping at the connection: a socket can be accepted
/// by something that then cannot talk to us, and the version is on the first answer anyway.
pub async fn probe(socket_path: &Path) -> ServiceAccess {
    let stream = match tokio::net::UnixStream::connect(socket_path).await {
        Ok(stream) => stream,
        Err(e) => return from_connect_error(socket_path, e),
    };

    let framed = LengthDelimitedCodec::builder().new_framed(stream);
    let transport = tarpc::serde_transport::new(framed, tokio_serde::formats::Json::default());
    let client = VpnRpcClient::new(tarpc::client::Config::default(), transport).spawn();

    let mut ctx = tarpc::context::current();
    ctx.deadline = std::time::Instant::now() + PROBE_DEADLINE;
    // `state_since(0, 0)` is answered immediately whatever the actor is doing: nought is no run
    // the service can be having, so it never waits on the hold.
    let published = match tokio::time::timeout(
        PROBE_DEADLINE.min(STATE_POLL_DEADLINE),
        client.state_since(ctx, 0, 0),
    )
    .await
    {
        Ok(Ok(published)) => published,
        // Answered by accepting and then not speaking. Nothing usable is there, and the caller's
        // move is the same as for an empty socket.
        Ok(Err(e)) => {
            debug!("the socket answered but the call failed: {e}");
            return ServiceAccess::Absent;
        }
        Err(_) => {
            debug!("the socket accepted the connection and then said nothing");
            return ServiceAccess::Absent;
        }
    };

    if published.protocol == PROTOCOL_VERSION {
        ServiceAccess::Available
    } else {
        ServiceAccess::WrongVersion {
            theirs: published.protocol,
        }
    }
}

/// Read a failed connect for what it says about *why*.
fn from_connect_error(socket_path: &Path, e: std::io::Error) -> ServiceAccess {
    use std::io::ErrorKind;

    match e.kind() {
        // The one that must not be mistaken for absence.
        ErrorKind::PermissionDenied => ServiceAccess::Forbidden {
            detail: e.to_string(),
        },
        // No file, or a file nothing is listening on any more.
        ErrorKind::NotFound | ErrorKind::ConnectionRefused => {
            debug!("no service at {}: {e}", socket_path.display());
            ServiceAccess::Absent
        }
        // Anything else — the path is a directory, the name is too long, the socket is of the
        // wrong kind. None of it is a service, and none of it is this client's to fix.
        _ => {
            debug!("nothing usable at {}: {e}", socket_path.display());
            ServiceAccess::Absent
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_path_with_nothing_at_it_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            probe(&dir.path().join("vpn.sock")).await,
            ServiceAccess::Absent
        );
    }

    /// The case a bare `Path::exists()` gets wrong: a process that died leaves its socket file
    /// behind, and the file is still there long after anything is listening.
    #[tokio::test]
    async fn a_socket_file_left_behind_by_a_dead_process_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vpn.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        drop(listener);

        assert!(path.exists(), "the file outlives the listener");
        assert_eq!(probe(&path).await, ServiceAccess::Absent);
    }

    /// Something is listening but never answers. A client must not hang on it, and must not
    /// mistake it for a service it can drive.
    #[tokio::test]
    async fn a_listener_that_never_answers_is_not_a_service() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vpn.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            // Accept and hold, saying nothing.
            let _held = listener.accept().await;
            std::future::pending::<()>().await;
        });

        assert_eq!(probe(&path).await, ServiceAccess::Absent);
    }

    /// The whole point of the module. A directory nobody may enter is how the socket is kept from
    /// the wrong users, and what comes back must say so rather than "there is nothing here".
    #[tokio::test]
    async fn a_socket_this_user_may_not_open_is_refused_not_missing() {
        use std::os::unix::fs::PermissionsExt;

        if unsafe { libc::geteuid() } == 0 {
            // Root is refused nothing, so there is nothing to observe.
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let closed = dir.path().join("closed");
        std::fs::create_dir(&closed).unwrap();
        let path = closed.join("vpn.sock");
        let _listener = tokio::net::UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).unwrap();

        let access = probe(&path).await;
        // Restore before asserting, so a failure does not leave an undeletable temp directory.
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert!(
            matches!(access, ServiceAccess::Forbidden { .. }),
            "being refused must not read as nothing being there, got {access:?}"
        );
        assert!(
            access.explain().is_some_and(|s| s.contains("floppa")),
            "and it must say what to do about it"
        );
    }

    #[test]
    fn the_answers_that_are_not_problems_have_nothing_to_explain() {
        assert!(ServiceAccess::Available.explain().is_none());
        assert!(ServiceAccess::Absent.explain().is_none());
        assert!(
            ServiceAccess::WrongVersion { theirs: 1 }
                .explain()
                .is_some_and(|s| s.contains("restart") || s.contains("Restart"))
        );
    }
}
