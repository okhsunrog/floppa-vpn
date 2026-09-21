//! The accept loop behind the `:vpn` process's RPC server.
//!
//! Kept apart from the tarpc plumbing in `rpc_server.rs` so that it compiles — and its tests run —
//! on the host: tarpc is an Android-only dependency, and the lifetime rule below is exactly the
//! kind of thing that must not be checked only on a phone.
//!
//! The rule: the loop runs for as long as its [`RpcServerHandle`] exists and has not been shut
//! down. Dropping the handle *is* a shutdown. Previously a dropped handle merely closed the watch
//! channel, and the loop read that as "not shut down yet" — so a `?` early exit in
//! `nativeStartServer` after a successful bind left a busy accept loop spinning on a worker thread
//! for the life of the process, holding a listener nobody could reach.

use std::path::{Path, PathBuf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Handle to a running accept loop. Drop or call `shutdown()` to stop it.
pub struct RpcServerHandle {
    shutdown_tx: watch::Sender<bool>,
    /// Cancels the tasks serving the connections this loop accepted. Handed to each of them, so
    /// they end with the generation that owns them.
    connections: CancellationToken,
    /// The path this loop bound, and may therefore unlink. `None` when the listener was inherited
    /// from systemd, which owns that file and re-creates it for the next start: removing it would
    /// take the socket out from under the unit and leave nothing for a client to connect to.
    socket_path: Option<PathBuf>,
}

impl RpcServerHandle {
    /// Stop accepting connections, and end the ones already accepted. The socket file is left in
    /// place.
    ///
    /// Leaving the file is deliberate: the path is shared between service generations, and by the
    /// time the previous generation is shut down the next one has usually already bound the same
    /// path (see `SERVER_GENERATION` in `jni_entry.rs`). Unlinking here would remove *its* socket.
    ///
    /// Ending the open connections is equally deliberate, and was missing: a task serving a
    /// connection held its own clone of the generation's state and answered from it long after
    /// the generation was gone, so a cached client in the UI process kept talking to a dead
    /// instance — it saw the wrong generation until the attempt budget ran out, and a stop sent
    /// down that connection stopped whichever service was live by then.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        self.connections.cancel();
    }

    /// Stop accepting connections and unlink the socket file.
    ///
    /// Only for the generation that owns the path — i.e. a `nativeStop` whose epoch matched — and
    /// never from a handle that has been superseded. A listener inherited from systemd owns no
    /// path and unlinks nothing.
    pub fn shutdown_and_unlink(self) {
        self.shutdown();
        let Some(path) = &self.socket_path else {
            debug!("the listener was inherited; leaving its socket to whoever made it");
            return;
        };
        match std::fs::remove_file(path) {
            Ok(()) => debug!("removed socket {}", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!("failed to remove socket {}: {e}", path.display()),
        }
    }
}

impl Drop for RpcServerHandle {
    /// A handle that goes away without an explicit `shutdown()` still ends the loop and its
    /// connections. The alternative — a loop that outlives every way of reaching it — is not a
    /// state worth keeping.
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        self.connections.cancel();
    }
}

/// Bind a Unix socket at `socket_path` and hand every accepted connection to `on_connect` until
/// the returned handle is shut down or dropped.
///
/// `on_connect` is given the connection *and* the generation's cancellation token, and must stop
/// serving when it fires: a connection that outlives its generation answers for a service instance
/// that no longer exists.
///
/// A stale socket file at the path is removed first. The loop runs as a task on the current Tokio
/// runtime, so this must be called from within one.
pub fn listen(
    socket_path: &Path,
    on_connect: impl FnMut(UnixStream, CancellationToken) + Send + 'static,
) -> std::io::Result<RpcServerHandle> {
    match std::fs::remove_file(socket_path) {
        Ok(()) => debug!("Removed stale socket: {}", socket_path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!(
            "Failed to remove stale socket {}: {e}",
            socket_path.display()
        ),
    }

    let listener = UnixListener::bind(socket_path)?;
    info!("tarpc server listening on {}", socket_path.display());
    Ok(spawn_accept_loop(
        listener,
        Some(socket_path.to_path_buf()),
        on_connect,
    ))
}

/// Serve on a listening socket systemd passed in, if it did.
///
/// `Ok(None)` means this process was not socket-activated and should bind for itself — which is
/// the ordinary case when the service is started by hand, and the reason this is an option rather
/// than an error.
///
/// Letting systemd own the socket is what keeps its ownership and mode out of this code: a
/// `.socket` unit states `SocketUser`, `SocketGroup` and `SocketMode`, and the file exists with
/// those from before the service starts. Doing it here instead would mean creating the file and
/// then widening it, with a window in between where it is reachable by the wrong people.
///
/// The handover is three environment variables and a fixed descriptor number, and this consumes
/// them: they describe *this* process, and a child that inherited them would believe it had been
/// handed the same socket.
#[cfg(target_os = "linux")]
pub fn inherited_listener() -> std::io::Result<Option<UnixListener>> {
    use std::os::fd::FromRawFd;

    /// systemd passes descriptors starting here, immediately after stdio.
    const LISTEN_FDS_START: i32 = 3;

    let fds = std::env::var("LISTEN_FDS").ok();
    let pid = std::env::var("LISTEN_PID").ok();
    // Consumed whatever happens next, including on the paths that decide to ignore them.
    unsafe {
        std::env::remove_var("LISTEN_FDS");
        std::env::remove_var("LISTEN_PID");
        std::env::remove_var("LISTEN_FDNAMES");
    }

    match read_handover(pid.as_deref(), fds.as_deref(), std::process::id()) {
        Handover::NotActivated(why) => {
            if let Some(why) = why {
                debug!("not socket-activated: {why}");
            }
            return Ok(None);
        }
        Handover::Wrong(why) => return Err(std::io::Error::other(why)),
        Handover::OneSocket => {}
    }

    // SAFETY: systemd guarantees descriptor 3 is an open listening socket when LISTEN_FDS says so
    // and LISTEN_PID names this process, both checked above. Nothing else in this process has
    // taken it: the variables are removed here, so this cannot run twice and hand out the same
    // descriptor to two owners.
    let std_listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(LISTEN_FDS_START) };
    // Tokio's reactor requires it, and systemd passes the descriptor blocking.
    std_listener.set_nonblocking(true)?;
    let listener = UnixListener::from_std(std_listener)?;
    info!("serving on the socket systemd passed in");
    Ok(Some(listener))
}

/// What `LISTEN_PID` and `LISTEN_FDS` say, before any descriptor is touched.
#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
enum Handover {
    /// Nobody handed us anything; bind for ourselves. The reason is worth a line when there was
    /// something to read and it did not apply to us.
    NotActivated(Option<String>),
    /// Exactly one listening socket, at `LISTEN_FDS_START`.
    OneSocket,
    /// Activated, but not in a way this service can act on. Refusing beats guessing: picking one
    /// of several sockets, or reading a descriptor number out of nonsense, would serve on
    /// something nobody meant.
    Wrong(String),
}

/// The decision, separated from the descriptor so every branch of it can be tested.
///
/// `me` is this process's pid, passed in for the same reason.
#[cfg(target_os = "linux")]
fn read_handover(listen_pid: Option<&str>, listen_fds: Option<&str>, me: u32) -> Handover {
    let (Some(pid), Some(fds)) = (listen_pid, listen_fds) else {
        return Handover::NotActivated(None);
    };
    // Addressed to a process that is not this one: systemd sets these before exec, so anything
    // that survived into a child says nothing about what *this* process was given.
    if pid.parse::<u32>().ok() != Some(me) {
        return Handover::NotActivated(Some(format!("LISTEN_PID is {pid}, not {me}")));
    }
    match fds.parse::<i32>() {
        Ok(0) => Handover::NotActivated(Some("LISTEN_FDS is 0".into())),
        Ok(1) => Handover::OneSocket,
        Ok(n) => Handover::Wrong(format!(
            "systemd passed {n} sockets; this service knows what to do with exactly one"
        )),
        Err(e) => Handover::Wrong(format!("LISTEN_FDS is not a number: {e}")),
    }
}

/// Serve on a listener this process did not bind, and must not unlink.
pub fn serve_on(
    listener: UnixListener,
    on_connect: impl FnMut(UnixStream, CancellationToken) + Send + 'static,
) -> RpcServerHandle {
    spawn_accept_loop(listener, None, on_connect)
}

fn spawn_accept_loop(
    listener: UnixListener,
    socket_path: Option<PathBuf>,
    mut on_connect: impl FnMut(UnixStream, CancellationToken) + Send + 'static,
) -> RpcServerHandle {
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let connections = CancellationToken::new();
    let handed_out = connections.clone();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                result = listener.accept() => match result {
                    Ok((stream, _addr)) => on_connect(stream, handed_out.clone()),
                    Err(e) => error!("Failed to accept connection: {e}"),
                },
                changed = shutdown_rx.changed() => {
                    // `Err` means the sender is gone: the handle was dropped, which is a shutdown
                    // too. Left unhandled, `changed()` returns `Err` immediately on every poll and
                    // this became a hot loop.
                    if changed.is_err() || *shutdown_rx.borrow() {
                        info!("tarpc server shutting down");
                        break;
                    }
                }
            }
        }
        // The socket file is *not* unlinked here. This task exits asynchronously after
        // `shutdown()`, and `shutdown()` is what `nativeStartServer` calls on the previous
        // generation right after binding the new one to the same path. An unlink from here raced
        // that bind and won often enough to remove the new generation's socket, leaving the UI
        // with `NotFound` until it gave up. Unlinking is done only before a bind (above) and by
        // the owning generation's `nativeStop` via [`RpcServerHandle::shutdown_and_unlink`].
    });

    RpcServerHandle {
        shutdown_tx,
        connections,
        socket_path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// A connect to a bound path whose listener is gone is refused rather than queued, which is
    /// how the tests observe the loop's end from the outside.
    async fn wait_until_refused(path: &Path) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while UnixStream::connect(path).await.is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the accept loop kept running");
    }

    #[tokio::test]
    async fn dropping_the_handle_ends_the_accept_loop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vpn.sock");
        let accepted = Arc::new(AtomicUsize::new(0));
        let counter = accepted.clone();
        let handle = listen(&path, move |_stream, _cancel| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();

        UnixStream::connect(&path).await.expect("the loop is up");
        tokio::time::timeout(Duration::from_secs(5), async {
            while accepted.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the connection was handed to the callback");

        drop(handle);
        wait_until_refused(&path).await;
        assert!(
            path.exists(),
            "the socket file is left for the owner to unlink"
        );
    }

    #[tokio::test]
    async fn shutdown_ends_the_accept_loop_and_unlink_removes_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vpn.sock");
        let handle = listen(&path, |_stream, _cancel| {}).unwrap();

        UnixStream::connect(&path).await.expect("the loop is up");
        handle.shutdown();
        wait_until_refused(&path).await;

        handle.shutdown_and_unlink();
        assert!(!path.exists());
    }

    /// The rule the zombie-connection bug broke: a connection accepted by a generation must not
    /// outlive it. Without this the task serving it kept answering from the old generation's
    /// state, and a client that had cached that connection never noticed the handover.
    #[tokio::test]
    async fn shutting_down_ends_the_connections_that_were_already_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vpn.sock");
        let accepted = Arc::new(AtomicUsize::new(0));
        let ended = Arc::new(AtomicUsize::new(0));
        let (seen, done) = (accepted.clone(), ended.clone());
        let handle = listen(&path, move |_stream, cancel| {
            seen.fetch_add(1, Ordering::SeqCst);
            let done = done.clone();
            tokio::spawn(async move {
                cancel.cancelled().await;
                done.fetch_add(1, Ordering::SeqCst);
            });
        })
        .unwrap();

        let _client = UnixStream::connect(&path).await.expect("the loop is up");
        wait_for(&accepted, "the connection was accepted").await;

        handle.shutdown();
        wait_for(&ended, "the accepted connection was told to stop").await;
    }

    /// The socket-activation handover, every branch of it. The descriptor itself is four lines
    /// that cannot be exercised without taking fd 3 away from whatever holds it; what can go
    /// wrong is all here.
    #[cfg(target_os = "linux")]
    mod handover {
        use super::super::{Handover, read_handover};

        #[test]
        fn nothing_set_means_bind_for_ourselves() {
            assert_eq!(read_handover(None, None, 42), Handover::NotActivated(None));
            assert_eq!(
                read_handover(Some("42"), None, 42),
                Handover::NotActivated(None)
            );
        }

        /// The variables are inherited by children, so seeing them is not the same as being the
        /// process they were meant for. A child that believed them would take a descriptor that
        /// is, in its own table, something else entirely.
        #[test]
        fn variables_meant_for_another_process_are_ignored() {
            let Handover::NotActivated(Some(why)) = read_handover(Some("41"), Some("1"), 42) else {
                panic!("a handover addressed to pid 41 must not be taken by pid 42");
            };
            assert!(why.contains("41"), "{why}");
        }

        #[test]
        fn one_socket_is_the_case_this_serves() {
            assert_eq!(
                read_handover(Some("42"), Some("1"), 42),
                Handover::OneSocket
            );
        }

        #[test]
        fn zero_sockets_is_not_an_error_it_is_just_nothing() {
            assert!(matches!(
                read_handover(Some("42"), Some("0"), 42),
                Handover::NotActivated(Some(_))
            ));
        }

        /// Refused rather than guessed at: serving on whichever descriptor came first would be
        /// serving on something nobody asked for.
        #[test]
        fn several_sockets_are_refused() {
            assert!(matches!(
                read_handover(Some("42"), Some("3"), 42),
                Handover::Wrong(_)
            ));
            assert!(matches!(
                read_handover(Some("42"), Some("banana"), 42),
                Handover::Wrong(_)
            ));
        }
    }

    async fn wait_for(counter: &AtomicUsize, what: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while counter.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting until {what}"));
    }
}
