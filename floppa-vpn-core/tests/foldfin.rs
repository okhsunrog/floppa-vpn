//! Client/server integration without altering host networking or reading production secrets.
use floppa_foldfin::{DeviceGrant, DeviceRegistry, TokenDigest};
use floppa_vpn_core::{protocol::Protocol, state::ProtocolConfig, tunnel::TunnelManager};
use foldfin::{
    packet::{PacketDevice, PacketDeviceFactory, PacketRecv, PacketSend},
    server::VpnConfig,
};
use std::{
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot},
    time::timeout,
};
const TOKEN: &str = "0123456789abcdef0123456789abcdef";

struct Rx(mpsc::Receiver<Vec<u8>>, Arc<AtomicUsize>);
struct Tx(mpsc::Sender<Vec<u8>>, Arc<AtomicUsize>);
impl Drop for Rx {
    fn drop(&mut self) {
        self.1.fetch_add(1, Ordering::SeqCst);
    }
}
impl Drop for Tx {
    fn drop(&mut self) {
        self.1.fetch_add(1, Ordering::SeqCst);
    }
}
impl PacketRecv for Rx {
    fn poll_recv(&mut self, cx: &mut Context<'_>, buffer: &mut [u8]) -> Poll<io::Result<usize>> {
        match std::task::ready!(self.0.poll_recv(cx)) {
            Some(packet) => {
                buffer[..packet.len()].copy_from_slice(&packet);
                Poll::Ready(Ok(packet.len()))
            }
            None => Poll::Ready(Ok(0)),
        }
    }
}
impl PacketSend for Tx {
    fn poll_send(&mut self, _: &mut Context<'_>, packet: &[u8]) -> Poll<io::Result<()>> {
        Poll::Ready(self.0.try_send(packet.to_vec()).map_err(io::Error::other))
    }
}
struct Factory(Mutex<std::collections::VecDeque<PacketDevice>>);
impl PacketDeviceFactory for Factory {
    fn open(&self) -> io::Result<PacketDevice> {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| io::Error::other("already opened"))
    }
}
fn packets(
    dropped: Arc<AtomicUsize>,
) -> (PacketDevice, mpsc::Sender<Vec<u8>>, mpsc::Receiver<Vec<u8>>) {
    let (input, recv) = mpsc::channel(16);
    let (send, output) = mpsc::channel(16);
    (
        PacketDevice::new(Rx(recv, dropped.clone()), Tx(send, dropped)),
        input,
        output,
    )
}
fn packet(source: [u8; 4], dest: [u8; 4]) -> Vec<u8> {
    let mut bytes = vec![0; 20];
    bytes[0] = 0x45;
    bytes[3] = 20;
    bytes[8] = 64;
    bytes[9] = 1;
    bytes[12..16].copy_from_slice(&source);
    bytes[16..20].copy_from_slice(&dest);
    bytes
}
fn document(endpoint: &str, ca: &str) -> String {
    serde_json::json!({"protocol":"foldfin", "config": {"version":1,"endpoint":endpoint,
        "server_name":"localhost","token":TOKEN,"address":"192.0.2.2/32","peer":"192.0.2.1",
        "routes":["192.0.2.1/32"],"dns":["192.0.2.1"],"mtu":1280,"chrome_tls":cfg!(feature = "foldfin-chrome"),"additional_ca_pem":ca}})
    .to_string()
}

fn tls() -> (Arc<foldfin::server::TlsConfig>, String) {
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("cert.pem");
    let key = dir.path().join("key.pem");
    assert!(
        std::process::Command::new("openssl")
            .args([
                "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-keyout"
            ])
            .arg(&key)
            .arg("-out")
            .arg(&cert)
            .args([
                "-subj",
                "/CN=localhost",
                "-addext",
                "subjectAltName=DNS:localhost",
                "-addext",
                "basicConstraints=critical,CA:FALSE"
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
    let ca = std::fs::read_to_string(cert).unwrap();
    let certs = rustls_pemfile::certs(&mut ca.as_bytes())
        .collect::<io::Result<Vec<_>>>()
        .unwrap();
    let key = rustls_pemfile::private_key(&mut std::fs::read(key).unwrap().as_slice())
        .unwrap()
        .unwrap();
    let mut tls = foldfin::server::TlsConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .unwrap();
    tls.alpn_protocols = vec![b"h2".to_vec()];
    (Arc::new(tls), ca)
}

#[test]
fn import_persistence_validation_and_secret_redaction() {
    let raw = document("localhost:8443", "");
    let mut store = floppa_vpn_core::store::ConfigStore::in_memory(Default::default());
    assert_eq!(store.import(&raw).unwrap(), Protocol::Foldfin);
    assert_eq!(store.available(), vec![Protocol::Foldfin]);
    let config = store.get(Protocol::Foldfin).unwrap();
    assert!(!format!("{config:?}").contains(TOKEN));
    let persisted = serde_json::to_string(&config).unwrap();
    assert_eq!(
        ProtocolConfig::parse(&persisted).unwrap().endpoint_str(),
        "localhost:8443"
    );
    let mut invalid: serde_json::Value = serde_json::from_str(&raw).unwrap();
    invalid["config"]["mtu"] = serde_json::json!(1501);
    assert!(ProtocolConfig::parse(&invalid.to_string()).is_err());
    invalid["config"]["mtu"] = serde_json::json!(1280);
    invalid["config"]["peer"] = serde_json::json!("192.0.2.2");
    assert!(ProtocolConfig::parse(&invalid.to_string()).is_err());
}

#[tokio::test]
async fn imported_foldfin_runs_on_manager_and_revocation_releases_packets() {
    let (tls, ca) = tls();
    let server_drops = Arc::new(AtomicUsize::new(0));
    let client_drops = Arc::new(AtomicUsize::new(0));
    let (device, server_input, mut server_output) = packets(server_drops.clone());
    let (successor_device, _successor_server_input, mut successor_server_output) =
        packets(server_drops.clone());
    let registry = Arc::new(DeviceRegistry::default());
    registry
        .install(DeviceGrant {
            device_id: "installation-1/eu".into(),
            token_digest: TokenDigest::from_token(TOKEN),
            expires_at: Instant::now() + Duration::from_secs(30),
            proxy: false,
            allow_private_proxy: false,
            vpn: Some(VpnConfig {
                policy: foldfin::vpn::Config {
                    local: vec!["192.0.2.1".parse().unwrap()],
                    peer: vec!["192.0.2.2".parse().unwrap()],
                    local_routes: vec![],
                    peer_routes: vec![],
                },
                device: Arc::new(Factory(Mutex::new(vec![device, successor_device].into()))),
            }),
        })
        .unwrap();
    let server = registry.server(tls).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    let (shutdown, stopped) = oneshot::channel();
    let server_task = tokio::spawn(async move {
        server
            .serve(listener, async {
                let _ = stopped.await;
            })
            .await
    });
    let ProtocolConfig::Foldfin(config) =
        ProtocolConfig::parse(&document(&endpoint.to_string(), &ca)).unwrap()
    else {
        panic!("wrong protocol")
    };
    let (device, input, mut output) = packets(client_drops.clone());
    let manager = TunnelManager::default();
    manager
        .start_foldfin_packets(&config, endpoint, device, None)
        .await
        .unwrap();
    timeout(Duration::from_secs(3), manager.probe())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(manager.meta().await.unwrap().protocol, Protocol::Foldfin);
    let request = packet([192, 0, 2, 2], [192, 0, 2, 1]);
    input.send(request.clone()).await.unwrap();
    assert_eq!(
        timeout(Duration::from_secs(3), server_output.recv())
            .await
            .unwrap()
            .unwrap(),
        request
    );
    let response = packet([192, 0, 2, 1], [192, 0, 2, 2]);
    server_input.send(response.clone()).await.unwrap();
    assert_eq!(
        timeout(Duration::from_secs(3), output.recv())
            .await
            .unwrap()
            .unwrap(),
        response
    );
    assert_eq!(manager.get_stats().await.unwrap().tx_bytes, 20);
    // A second authenticated carrier takes over the same device after roaming,
    // waiting for old packet IO ownership rather than rejecting it by source IP.
    let successor = TunnelManager::default();
    let (device, successor_input, _successor_output) = packets(client_drops.clone());
    successor
        .start_foldfin_packets(&config, endpoint, device, None)
        .await
        .unwrap();
    timeout(Duration::from_secs(3), successor.probe())
        .await
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(3), async {
        while manager.meta().await.is_some() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let _ = manager.stop().await;
    assert_eq!(client_drops.load(Ordering::SeqCst), 2);
    successor_input.send(request.clone()).await.unwrap();
    assert_eq!(
        timeout(Duration::from_secs(3), successor_server_output.recv())
            .await
            .unwrap()
            .unwrap(),
        request
    );
    registry.revoke("installation-1/eu");
    timeout(Duration::from_secs(3), async {
        while successor.meta().await.is_some() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let _ = successor.stop().await; // previous session failure propagates, while teardown still joins
    assert_eq!(client_drops.load(Ordering::SeqCst), 4);
    shutdown.send(()).unwrap();
    server_task.await.unwrap().unwrap();
    assert_eq!(server_drops.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn backend_checks_each_connect_rotation_expiry_and_mode() {
    use foldfin::{
        client::{Client, ClientConfig, TlsProfile},
        credential::Credential,
    };
    use tokio::{io::AsyncReadExt, net::TcpStream};
    let (tls, ca) = tls();
    let registry = Arc::new(DeviceRegistry::default());
    let install = |token: &str, deadline: Instant, proxy: bool| {
        registry
            .install(DeviceGrant {
                device_id: "device".into(),
                token_digest: TokenDigest::from_token(token),
                expires_at: deadline,
                proxy,
                allow_private_proxy: true,
                vpn: None,
            })
            .unwrap()
    };
    install(TOKEN, Instant::now() + Duration::from_secs(30), true);
    let server = registry.server(tls).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown, stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        server
            .serve(listener, async {
                let _ = stopped.await;
            })
            .await
    });
    let dial = |token: &str| ClientConfig {
        authority: "localhost:8443".into(),
        server_name: "localhost".into(),
        credential: Credential::bearer(token).unwrap(),
        additional_ca_pem: Some(ca.as_bytes().to_vec()),
        tls_profile: TlsProfile::Rustls,
    };
    let client = Client::connect_over(dial(TOKEN), TcpStream::connect(address).await.unwrap())
        .await
        .unwrap();
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = upstream.local_addr().unwrap().to_string();
    let bad = Client::connect_over(
        dial("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"),
        TcpStream::connect(address).await.unwrap(),
    )
    .await
    .unwrap();
    assert!(bad.open_tcp(&target).await.is_err());
    bad.stop().await.unwrap();
    let flow = client.open_tcp(&target).await.unwrap();
    let (mut socket, _) = upstream.accept().await.unwrap();
    let replacement = "abcdef0123456789abcdef0123456789";
    install(replacement, Instant::now() + Duration::from_secs(30), true);
    assert_eq!(
        timeout(Duration::from_secs(3), socket.read(&mut [0; 1]))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    drop(flow);
    // Same established TLS/H2 carrier cannot keep opening flows using a revoked credential.
    assert!(client.open_tcp(&target).await.is_err());
    client.stop().await.unwrap();
    let client = Client::connect_over(
        dial(replacement),
        TcpStream::connect(address).await.unwrap(),
    )
    .await
    .unwrap();
    let flow = client.open_tcp(&target).await.unwrap();
    let (mut socket, _) = upstream.accept().await.unwrap();
    // Confirm that a routine lease renewal preserves an existing flow.
    registry
        .renew("device", Instant::now() + Duration::from_millis(100))
        .unwrap();
    registry
        .renew("device", Instant::now() + Duration::from_secs(30))
        .unwrap();
    assert!(
        timeout(Duration::from_millis(150), socket.read(&mut [0; 1]))
            .await
            .is_err()
    );
    // Rotation also changes permitted modes, immediately cancelling existing flows.
    install(replacement, Instant::now() + Duration::from_secs(30), false);
    assert_eq!(
        timeout(Duration::from_secs(3), socket.read(&mut [0; 1]))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    drop(flow);
    assert!(client.open_tcp(&target).await.is_err());
    install(replacement, Instant::now() + Duration::from_secs(1), true);
    let flow = client.open_tcp(&target).await.unwrap();
    let (mut socket, _) = upstream.accept().await.unwrap();
    assert_eq!(
        timeout(Duration::from_secs(3), socket.read(&mut [0; 1]))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    drop(flow);
    assert!(client.open_tcp(&target).await.is_err());
    client.stop().await.unwrap();
    registry.prune_expired();
    shutdown.send(()).unwrap();
    task.await.unwrap().unwrap();
}
