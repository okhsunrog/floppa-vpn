//! Run through tests/integration/test_foldfin_underlay.py, never on the host route.
use floppa_vpn_core::underlay::{NativeUnderlay, Underlay};
use std::{io, net::SocketAddr, sync::Arc, time::Duration};

#[tokio::main]
async fn main() -> io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("floppa_net=debug")
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .init();
    let mut args = std::env::args().skip(1);
    let host = args.next().expect("HOST MARKER_IP:PORT");
    let marker: SocketAddr = args.next().expect("MARKER_IP:PORT").parse().unwrap();
    let underlay = Arc::new(NativeUnderlay);
    // No persistent cache is initialized: this must obtain fresh DNS answers.
    let addresses = underlay
        .clone()
        .resolve(host.clone(), format!("underlay-acceptance:{host}"))
        .await?;
    let socket = if marker.is_ipv4() {
        tokio::net::TcpSocket::new_v4()?
    } else {
        tokio::net::TcpSocket::new_v6()?
    };
    underlay.prepare_tcp(&socket)?;
    let socket = tokio::time::timeout(Duration::from_secs(10), socket.connect(marker)).await??;
    println!(
        "{}",
        serde_json::json!({
            "addresses": addresses, "marker_local": socket.local_addr()?.to_string(),
            "marker_peer": socket.peer_addr()?.to_string()
        })
    );
    // Keep the source port owned until the harness verifies it on the server.
    tokio::time::sleep(Duration::from_secs(45)).await;
    drop(socket);
    Ok(())
}
