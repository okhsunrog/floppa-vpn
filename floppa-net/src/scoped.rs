//! Private one-destination relay for HTTPS on a host-prepared socket.
//! No upstream byte is sent before preparation succeeds.
use std::{io, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpSocket},
};

pub type SocketPreparation = Arc<dyn Fn(&TcpSocket) -> io::Result<()> + Send + Sync>;
struct Relay(tokio::task::JoinHandle<()>);
impl Drop for Relay {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub async fn client(
    endpoint: SocketAddr,
    prepare: &SocketPreparation,
) -> io::Result<(reqwest::Client, impl Clone + Send + Sync + 'static + use<>)> {
    let socket = if endpoint.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    prepare(&socket)?;
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
            let mut upstream = socket.connect(endpoint).await?;
            downstream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
            tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await?;
            Ok::<_, io::Error>(())
        };
        match tokio::time::timeout(Duration::from_secs(25), serve).await {
            Ok(Err(error)) => tracing::debug!(%endpoint, %error, "prepared HTTPS relay failed"),
            Err(_) => tracing::debug!(%endpoint, "prepared HTTPS relay timed out"),
            Ok(Ok(())) => {}
        }
    });
    let guard = Arc::new(Relay(task));
    let client = super::client_builder()
        .no_proxy()
        .proxy(proxy)
        .build()
        .map_err(io::Error::other)?;
    Ok((client, guard))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn preparation_refusal_never_connects() {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let prepare: SocketPreparation = Arc::new(|_| Err(io::ErrorKind::PermissionDenied.into()));
        let result = client(upstream.local_addr().unwrap(), &prepare).await;
        assert!(matches!(result, Err(e) if e.kind() == io::ErrorKind::PermissionDenied));
        assert!(
            tokio::time::timeout(Duration::from_millis(30), upstream.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn dropping_request_guard_closes_upstream() {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let prepare: SocketPreparation = Arc::new(|_| Ok(()));
        let (client, guard) = client(upstream.local_addr().unwrap(), &prepare)
            .await
            .unwrap();
        let request =
            tokio::spawn(async move { client.get("https://example.invalid/").send().await });
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(2), upstream.accept())
            .await
            .unwrap()
            .unwrap();
        let mut bytes = [0; 8192];
        assert!(socket.read(&mut bytes).await.unwrap() > 0); // TLS ClientHello, no DNS.
        drop(guard);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), socket.read(&mut bytes))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        assert!(request.await.unwrap().is_err());
    }
}
