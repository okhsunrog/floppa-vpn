//! A private, single-destination CONNECT relay lets reqwest retain its TLS implementation while
//! we bind its upstream socket to an Android Network before connect. WebView never uses this.
use std::{
    io,
    net::SocketAddr,
    os::fd::AsRawFd,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
static NETWORK: AtomicU64 = AtomicU64::new(0);
pub fn set_network(handle: u64) {
    if NETWORK.swap(handle, Ordering::AcqRel) != handle {
        super::network_changed();
    }
}

#[link(name = "android")]
unsafe extern "C" {
    fn android_setsocknetwork(network: u64, fd: i32) -> i32;
}

pub async fn client(
    endpoint: SocketAddr,
) -> io::Result<(reqwest::Client, impl Clone + Send + Sync + 'static)> {
    let prepare: super::scoped::SocketPreparation = Arc::new(|socket| {
        let network = NETWORK.load(Ordering::Acquire);
        if network == 0 {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "no underlying network",
            ));
        }
        // SAFETY: this is an owned, live socket. The API does not retain its descriptor.
        if unsafe { android_setsocknetwork(network, socket.as_raw_fd()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    });
    super::scoped::client(endpoint, &prepare).await
}
