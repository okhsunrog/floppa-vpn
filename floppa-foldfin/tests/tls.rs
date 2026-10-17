use floppa_foldfin::tls::TlsIdentity;
use foldfin::{
    client::ClientConfig,
    credential::Credential,
    server::{Server, ServerConfig},
    session::Carrier,
};
use std::{
    net::{SocketAddr, UdpSocket},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

mod support;
use support::certificate;

async fn connect(
    h3: bool,
    address: SocketAddr,
    certificate: &[u8],
) -> foldfin::error::Result<Carrier> {
    let config = ClientConfig {
        authority: address.to_string(),
        server_name: "localhost".into(),
        credential: Credential::bearer(TOKEN).unwrap(),
        additional_ca_pem: Some(certificate.to_vec()),
    };
    if h3 {
        foldfin::h3_client::Client::connect_over(
            config,
            UdpSocket::bind("127.0.0.1:0").unwrap(),
            address,
        )
        .await
        .map(Carrier::H3)
    } else {
        foldfin::client::Client::connect_over(config, TcpStream::connect(address).await?)
            .await
            .map(Carrier::H2)
    }
}

#[tokio::test]
async fn both_carriers_reload_identity_without_disconnecting_existing_flows() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (old_cert, old_key) = certificate();
        let (new_cert, new_key) = certificate();
        let identity = Arc::new(TlsIdentity::from_pem(&old_cert, &old_key).unwrap());
        let server = Server::new(ServerConfig {
            tls: identity.server_config(),
            credential: Credential::bearer(TOKEN).unwrap(),
            allow_private: true,
            vpn: None,
        })
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let udp = UdpSocket::bind(address).unwrap();
        let (stop, mut h2_stop) = watch::channel(false);
        let mut h3_stop = h2_stop.clone();
        let h2_server = server.clone();
        let h2 = tokio::spawn(async move {
            h2_server
                .serve(listener, async {
                    let _ = h2_stop.wait_for(|stop| *stop).await;
                })
                .await
        });
        let h3 = tokio::spawn(async move {
            server
                .serve_h3(udp, async {
                    let _ = h3_stop.wait_for(|stop| *stop).await;
                })
                .await
        });
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = echo.local_addr().unwrap().to_string();
        let echo = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            for _ in 0..2 {
                let (mut stream, _) = echo.accept().await.unwrap();
                tasks.spawn(async move {
                    let mut buffer = [0; 1];
                    while stream.read(&mut buffer).await.unwrap() != 0 {
                        stream.write_all(&buffer).await.unwrap();
                    }
                });
            }
            while let Some(task) = tasks.join_next().await {
                task.unwrap();
            }
        });
        let mut held = Vec::new();
        for h3 in [false, true] {
            let carrier = connect(h3, address, &old_cert).await.unwrap();
            let mut flow = carrier.open_tcp(&target).await.unwrap();
            flow.write_all(b"a").await.unwrap();
            flow.flush().await.unwrap();
            assert_eq!(flow.read_u8().await.unwrap(), b'a');
            held.push((carrier, flow));
        }
        assert!(identity.replace_pem(&new_cert, &old_key).is_err());
        assert!(identity.replace_pem(b"invalid", &new_key).is_err());
        for h3 in [false, true] {
            connect(h3, address, &old_cert)
                .await
                .unwrap()
                .stop()
                .await
                .unwrap();
        }
        identity.replace_pem(&new_cert, &new_key).unwrap();
        for h3 in [false, true] {
            assert!(connect(h3, address, &old_cert).await.is_err());
            connect(h3, address, &new_cert)
                .await
                .unwrap()
                .stop()
                .await
                .unwrap();
        }
        for (carrier, mut flow) in held {
            flow.write_all(b"b").await.unwrap();
            flow.flush().await.unwrap();
            assert_eq!(flow.read_u8().await.unwrap(), b'b');
            flow.shutdown().await.unwrap();
            let mut tail = Vec::new();
            flow.read_to_end(&mut tail).await.unwrap();
            assert!(tail.is_empty());
            carrier.stop().await.unwrap();
        }
        stop.send_replace(true);
        h2.await.unwrap().unwrap();
        h3.await.unwrap().unwrap();
        echo.await.unwrap();
    })
    .await
    .unwrap();
}
