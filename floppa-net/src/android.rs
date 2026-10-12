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
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpSocket},
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

struct Relay(tokio::task::JoinHandle<()>);
impl Drop for Relay {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub async fn client(
    endpoint: SocketAddr,
) -> io::Result<(reqwest::Client, impl Clone + Send + Sync + 'static)> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let proxy = reqwest::Proxy::all(format!("http://{}", listener.local_addr()?))
        .map_err(io::Error::other)?;
    let task = tokio::spawn(async move {
        let serve = async {
            let (mut downstream, _) = listener.accept().await?;
            let mut header = Vec::new();
            loop {
                if header.len() >= 8192 {
                    return Err(io::Error::other("proxy header too large"));
                }
                header.push(downstream.read_u8().await?);
                if header.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            if !header.starts_with(b"CONNECT ") {
                return Err(io::Error::other("HTTPS CONNECT required"));
            }
            let socket = if endpoint.is_ipv4() {
                TcpSocket::new_v4()?
            } else {
                TcpSocket::new_v6()?
            };
            let network = NETWORK.load(Ordering::Acquire);
            if network == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "no underlying network",
                ));
            }
            // SAFETY: this is an owned, live socket. The NDK API does not retain its descriptor.
            if unsafe { android_setsocknetwork(network, socket.as_raw_fd()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut upstream = socket.connect(endpoint).await?;
            downstream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
            tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await?;
            Ok::<_, io::Error>(())
        };
        let _ = tokio::time::timeout(Duration::from_secs(25), serve).await;
    });
    let guard = Arc::new(Relay(task));
    let client = super::client_builder()
        .no_proxy()
        .proxy(proxy)
        .build()
        .map_err(io::Error::other)?;
    Ok((client, guard))
}
