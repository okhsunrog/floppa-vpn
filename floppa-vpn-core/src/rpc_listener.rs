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
    socket_path: PathBuf,
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
    /// never from a handle that has been superseded.
    pub fn shutdown_and_unlink(self) {
        self.shutdown();
        match std::fs::remove_file(&self.socket_path) {
            Ok(()) => debug!("removed socket {}", self.socket_path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!(
                "failed to remove socket {}: {e}",
                self.socket_path.display()
            ),
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
    mut on_connect: impl FnMut(UnixStream, CancellationToken) + Send + 'static,
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

    Ok(RpcServerHandle {
        shutdown_tx,
        connections,
        socket_path: socket_path.to_path_buf(),
    })
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
