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
use tracing::{debug, error, info, warn};

/// Handle to a running accept loop. Drop or call `shutdown()` to stop it.
pub struct RpcServerHandle {
    shutdown_tx: watch::Sender<bool>,
    socket_path: PathBuf,
}

impl RpcServerHandle {
    /// Stop accepting connections. The socket file is left in place.
    ///
    /// Deliberately: the path is shared between service generations, and by the time the previous
    /// generation is shut down the next one has usually already bound the same path (see
    /// `SERVER_EPOCH` in `jni_entry.rs`). Unlinking here would remove *its* socket.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
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
    /// A handle that goes away without an explicit `shutdown()` still ends the loop. The
    /// alternative — a loop that outlives every way of reaching it — is not a state worth keeping.
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
    }
}

/// Bind a Unix socket at `socket_path` and hand every accepted connection to `on_connect` until
/// the returned handle is shut down or dropped.
///
/// A stale socket file at the path is removed first. The loop runs as a task on the current Tokio
/// runtime, so this must be called from within one.
pub fn listen(
    socket_path: &Path,
    mut on_connect: impl FnMut(UnixStream) + Send + 'static,
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

    tokio::spawn(async move {
        loop {
            tokio::select! {
                result = listener.accept() => match result {
                    Ok((stream, _addr)) => on_connect(stream),
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
        let handle = listen(&path, move |_stream| {
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
        let handle = listen(&path, |_stream| {}).unwrap();

        UnixStream::connect(&path).await.expect("the loop is up");
        handle.shutdown();
        wait_until_refused(&path).await;

        handle.shutdown_and_unlink();
        assert!(!path.exists());
    }
}
