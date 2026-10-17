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
    serde_json::json!({"protocol":"foldfin", "config": {"version":2,"endpoints":[endpoint],"transport":"h2_only",
        "server_name":"localhost","token":TOKEN,"network":{"revision":1,"addresses":["192.0.2.2/32"],"peers":["192.0.2.1"],
        "routes":["192.0.2.1/32"],"dns":["192.0.2.1"],"mtu":1280},"additional_ca_pem":ca}})
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
    invalid["config"]["network"]["mtu"] = serde_json::json!(1501);
    assert!(ProtocolConfig::parse(&invalid.to_string()).is_err());
    invalid["config"]["network"]["mtu"] = serde_json::json!(1280);
    invalid["config"]["network"]["peers"] = serde_json::json!(["192.0.2.2"]);
    assert!(ProtocolConfig::parse(&invalid.to_string()).is_err());
}

#[tokio::test]
async fn imported_foldfin_runs_on_manager_and_revocation_releases_packets() {
    manager_lifecycle("h2_only", false, false).await;
}
#[tokio::test]
async fn h3_manager_recovers_and_revokes_without_recreating_device() {
    manager_lifecycle("h3_only", true, false).await;
}
#[tokio::test]
async fn auto_falls_back_when_udp_is_unresponsive() {
    manager_lifecycle("auto", false, false).await;
}
#[tokio::test]
async fn recovery_resolves_provisioned_endpoints_without_recreating_device() {
    manager_lifecycle("h2_only", false, true).await;
}
async fn manager_lifecycle(transport: &str, h3: bool, discovery: bool) {
    let (tls, ca) = tls();
    let server_drops = Arc::new(AtomicUsize::new(0));
    let client_drops = Arc::new(AtomicUsize::new(0));
    let (device, server_input, mut server_output) = packets(server_drops.clone());
    let (successor_device, _successor_server_input, mut successor_server_output) =
        packets(server_drops.clone());
    let (recovery_device, _recovery_input, mut recovery_output) = packets(server_drops.clone());
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
                device: Arc::new(Factory(Mutex::new(
                    vec![device, recovery_device, successor_device].into(),
                ))),
            }),
        })
        .unwrap();
    let server = registry.server(tls).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    // Bound but unread UDP models a path blackhole, without relying on ICMP.
    let udp = std::net::UdpSocket::bind(endpoint).unwrap();
    let (shutdown, stopped) = oneshot::channel();
    let server_task = tokio::spawn(async move {
        if h3 {
            return server
                .serve_h3(udp, async {
                    let _ = stopped.await;
                })
                .await;
        }
        let _blackhole = udp;
        server
            .serve(listener, async {
                let _ = stopped.await;
            })
            .await
    });
    let mut raw: serde_json::Value =
        serde_json::from_str(&document(&endpoint.to_string(), &ca)).unwrap();
    raw["config"]["transport"] = serde_json::json!(transport);
    if discovery {
        raw["config"]["endpoints"] = serde_json::json!([
            format!("unavailable.invalid:{}", endpoint.port()),
            format!("recovered.invalid:{}", endpoint.port())
        ]);
    }
    let ProtocolConfig::Foldfin(config) = ProtocolConfig::parse(&raw.to_string()).unwrap() else {
        panic!("wrong protocol")
    };
    let (device, input, mut output) = packets(client_drops.clone());
    struct Prepared {
        tcp: Arc<AtomicUsize>,
        udp: Arc<AtomicUsize>,
    }
    impl floppa_vpn_core::underlay::Underlay for Prepared {
        fn resolve(
            self: Arc<Self>,
            host: String,
            _: String,
        ) -> std::pin::Pin<Box<dyn Future<Output = io::Result<Vec<std::net::IpAddr>>> + Send>>
        {
            Box::pin(async move {
                Ok(match host.as_str() {
                    "unavailable.invalid" => vec!["127.0.0.2".parse().unwrap()],
                    "recovered.invalid" => {
                        vec!["127.0.0.3".parse().unwrap(), "127.0.0.1".parse().unwrap()]
                    }
                    _ => vec![host.parse().unwrap()],
                })
            })
        }
        fn service_generation(&self) -> Option<u64> {
            Some(42)
        }
        fn prepare_tcp(&self, _: &tokio::net::TcpSocket) -> io::Result<()> {
            self.tcp.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn prepare_udp(&self, _: &std::net::UdpSocket) -> io::Result<()> {
            self.udp.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    let prepared_tcp = Arc::new(AtomicUsize::new(0));
    let prepared_udp = Arc::new(AtomicUsize::new(0));
    let underlay = Arc::new(Prepared {
        tcp: prepared_tcp.clone(),
        udp: prepared_udp.clone(),
    });
    let manager = TunnelManager::default();
    manager
        .start_foldfin_packets_on(&config, endpoint, device, underlay)
        .await
        .unwrap();
    timeout(Duration::from_secs(3), manager.probe())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(manager.meta().await.unwrap().protocol, Protocol::Foldfin);
    assert_eq!(manager.get_last_packet_received().await, None);
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
    // A stale service notification cannot start recovery on this session.
    manager.notify_network_changed_for(999).await.unwrap();
    assert_eq!(prepared_tcp.load(Ordering::SeqCst), if h3 { 0 } else { 1 });
    assert_eq!(
        prepared_udp.load(Ordering::SeqCst),
        if transport == "h2_only" { 0 } else { 1 }
    );
    // Notification invalidates the carrier; only the actor starts a new attempt.
    manager.notify_network_changed_for(42).await.unwrap();
    assert_eq!(
        manager.carrier_health().await,
        Some(floppa_vpn_core::actor::types::CarrierHealth::Lost)
    );
    assert_eq!(client_drops.load(Ordering::SeqCst), 0);
    assert_eq!(prepared_tcp.load(Ordering::SeqCst), if h3 { 0 } else { 1 });
    // Recovery replaces only the carrier, preserving the client packet device.
    timeout(Duration::from_secs(3), manager.recover_carrier())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(client_drops.load(Ordering::SeqCst), 0);
    assert_eq!(
        prepared_tcp.load(Ordering::SeqCst),
        if h3 {
            0
        } else if discovery {
            4
        } else {
            2
        }
    );
    if discovery {
        assert_eq!(
            manager.meta().await.unwrap().endpoint,
            format!("recovered.invalid:{}", endpoint.port())
        );
        assert_eq!(
            config.endpoint(),
            format!("unavailable.invalid:{}", endpoint.port())
        );
    }
    assert_eq!(
        prepared_udp.load(Ordering::SeqCst),
        if transport == "h2_only" { 0 } else { 2 }
    );
    input.send(request.clone()).await.unwrap();
    assert_eq!(
        timeout(Duration::from_secs(3), recovery_output.recv())
            .await
            .unwrap()
            .unwrap(),
        request
    );
    // A second authenticated carrier takes over the same device after roaming,
    // waiting for old packet IO ownership rather than rejecting it by source IP.
    let successor = TunnelManager::default();
    let (device, successor_input, _successor_output) = packets(client_drops.clone());
    successor
        .start_foldfin_packets_on(&config, endpoint, device, Arc::new(LoopbackUnderlay))
        .await
        .unwrap();
    timeout(Duration::from_secs(3), successor.probe())
        .await
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(3), async {
        while manager.probe().await.is_ok() {
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
    // Failed device preparation must not revoke the currently forwarding generation.
    // The factory has no device left for this candidate.
    let failed = TunnelManager::default();
    let (device, _input, _output) = packets(Arc::new(AtomicUsize::new(0)));
    assert!(
        failed
            .start_foldfin_packets_on(&config, endpoint, device, Arc::new(LoopbackUnderlay))
            .await
            .is_err()
    );
    successor.probe().await.unwrap();
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
        while successor.probe().await.is_ok() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let _ = successor.stop().await; // previous session failure propagates, while teardown still joins
    assert_eq!(client_drops.load(Ordering::SeqCst), 4);
    shutdown.send(()).unwrap();
    server_task.await.unwrap().unwrap();
    assert_eq!(server_drops.load(Ordering::SeqCst), 6);
}

#[tokio::test]
async fn backend_checks_each_connect_rotation_expiry_and_mode() {
    use foldfin::{
        client::{Client, ClientConfig},
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

#[test]
fn v2_bootstrap_plan_preserves_dual_stack_routes_and_exclusions() {
    use floppa_vpn_core::{
        actor::types::{SplitMode, TunnelParams},
        autostart::TunSpec,
    };
    let mut document: serde_json::Value =
        serde_json::from_str(&document("localhost:8443", "")).unwrap();
    document["config"]["network"]["addresses"] = serde_json::json!(["192.0.2.2/32", "fd42::2/128"]);
    document["config"]["network"]["peers"] = serde_json::json!(["192.0.2.1", "fd42::1"]);
    document["config"]["network"]["routes"] = serde_json::json!(["192.0.2.1/32", "2001:db8::/32"]);
    document["config"]["network"]["excluded_routes"] = serde_json::json!(["2001:db8:1::/48"]);
    let config = ProtocolConfig::parse(&document.to_string()).unwrap();
    let spec = TunSpec::derive(&config, &TunnelParams::new(SplitMode::All, vec![]));
    assert_eq!(spec.ipv4_addr, "192.0.2.2/32");
    assert_eq!(spec.ipv6_addr.as_deref(), Some("fd42::2/128"));
    let routes: Vec<ipnetwork::IpNetwork> =
        spec.routes.iter().map(|s| s.parse().unwrap()).collect();
    for (address, expected) in [
        ("192.0.2.1", true),
        ("8.8.8.8", false),
        ("2001:db8:2::1", true),
        ("2001:db8:1::1", false),
    ] {
        assert_eq!(
            routes
                .iter()
                .any(|route| route.contains(address.parse().unwrap())),
            expected,
            "{address}"
        );
    }
    document["config"]["version"] = serde_json::json!(1);
    assert!(
        ProtocolConfig::parse(&document.to_string())
            .unwrap_err()
            .to_string()
            .contains("reissue")
    );
}

#[tokio::test]
async fn auto_does_not_try_h2_after_h3_certificate_failure() {
    let (server_tls, _) = tls();
    let (_, unrelated_ca) = tls();
    let server = Arc::new(DeviceRegistry::default())
        .server(server_tls)
        .unwrap();
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = tcp.local_addr().unwrap();
    let udp = std::net::UdpSocket::bind(endpoint).unwrap();
    let (stop, stopped) = oneshot::channel();
    let server_task = tokio::spawn(async move {
        server
            .serve_h3(udp, async {
                let _ = stopped.await;
            })
            .await
    });
    let mut raw: serde_json::Value =
        serde_json::from_str(&document(&endpoint.to_string(), &unrelated_ca)).unwrap();
    raw["config"]["transport"] = serde_json::json!("auto");
    let ProtocolConfig::Foldfin(config) = ProtocolConfig::parse(&raw.to_string()).unwrap() else {
        panic!("wrong protocol")
    };
    let dropped = Arc::new(AtomicUsize::new(0));
    let (device, _input, _output) = packets(dropped.clone());
    let manager = TunnelManager::default();
    let error = manager
        .start_foldfin_packets_on(&config, endpoint, device, Arc::new(LoopbackUnderlay))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("TLS"), "{error}");
    assert!(matches!(
        error,
        floppa_vpn_core::backend::BackendError::ConnectionRejected { .. }
    ));
    assert!(
        timeout(Duration::from_millis(100), tcp.accept())
            .await
            .is_err(),
        "certificate failure must not attempt H2"
    );
    assert_eq!(dropped.load(Ordering::SeqCst), 2);
    stop.send(()).unwrap();
    server_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn underlay_refusal_precedes_any_tcp_or_udp_transmission() {
    struct Deny;
    impl floppa_vpn_core::underlay::Underlay for Deny {
        fn prepare_tcp(&self, _: &tokio::net::TcpSocket) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "test underlay refused",
            ))
        }
        fn prepare_udp(&self, _: &std::net::UdpSocket) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "test underlay refused",
            ))
        }
    }
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = tcp.local_addr().unwrap();
    let udp = tokio::net::UdpSocket::bind(endpoint).await.unwrap();
    for transport in ["h2_only", "h3_only", "auto"] {
        let mut raw: serde_json::Value =
            serde_json::from_str(&document(&endpoint.to_string(), "")).unwrap();
        raw["config"]["transport"] = serde_json::json!(transport);
        let ProtocolConfig::Foldfin(config) = ProtocolConfig::parse(&raw.to_string()).unwrap()
        else {
            panic!("wrong protocol")
        };
        let dropped = Arc::new(AtomicUsize::new(0));
        let (device, _input, _output) = packets(dropped.clone());
        let manager = TunnelManager::default();
        let error = manager
            .start_foldfin_packets_on(&config, endpoint, device, Arc::new(Deny))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("test underlay refused"),
            "{error}"
        );
        assert!(matches!(
            error,
            floppa_vpn_core::backend::BackendError::PermissionDenied { .. }
        ));
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
    }
    assert!(
        timeout(Duration::from_millis(100), tcp.accept())
            .await
            .is_err()
    );
    assert!(
        timeout(Duration::from_millis(100), udp.recv(&mut [0; 1500]))
            .await
            .is_err()
    );
}

// Packet-channel tests have no host routing; production uses mandatory marked sockets.
struct LoopbackUnderlay;
impl floppa_vpn_core::underlay::Underlay for LoopbackUnderlay {
    fn prepare_tcp(&self, _: &tokio::net::TcpSocket) -> io::Result<()> {
        Ok(())
    }
    fn prepare_udp(&self, _: &std::net::UdpSocket) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn endpoint_cache_tracks_trust_identity_without_tracking_credentials() {
    let mut raw: serde_json::Value =
        serde_json::from_str(&document("localhost:8443", "ca-one")).unwrap();
    let identity = |v: &serde_json::Value| {
        ProtocolConfig::parse(&v.to_string())
            .unwrap()
            .endpoint_cache_identity()
    };
    let original = identity(&raw);
    assert!(!original.contains(TOKEN));
    raw["config"]["token"] = serde_json::json!("rotatedcredential0123456789abcdef012345");
    assert_eq!(identity(&raw), original);
    raw["config"]["server_name"] = serde_json::json!("other.example");
    assert_ne!(identity(&raw), original);
    raw["config"]["server_name"] = serde_json::json!("localhost");
    raw["config"]["additional_ca_pem"] = serde_json::json!("ca-two");
    assert_ne!(identity(&raw), original);
}

#[test]
fn provisioned_exclusions_compose_with_shared_user_exclusions() {
    let mut raw: serde_json::Value = serde_json::from_str(&document("localhost:8443", "")).unwrap();
    raw["config"]["network"]["routes"] = serde_json::json!(["0.0.0.0/0"]);
    raw["config"]["network"]["excluded_routes"] = serde_json::json!(["10.0.0.0/8"]);
    let config = ProtocolConfig::parse(&raw.to_string()).unwrap();
    let routes = floppa_tunnel_config::route::exclude_networks(
        config.allowed_ips_networks(),
        &["172.16.0.0/12".parse().unwrap()],
    );
    let carries = |ip: &str| routes.iter().any(|r| r.contains(ip.parse().unwrap()));
    assert!(!carries("10.2.3.4"));
    assert!(!carries("172.18.1.1"));
    assert!(carries("192.0.2.1"));
}

#[tokio::test]
async fn refused_tcp_connection_is_an_endpoint_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    drop(listener);
    let mut raw: serde_json::Value =
        serde_json::from_str(&document(&endpoint.to_string(), "")).unwrap();
    raw["config"]["transport"] = serde_json::json!("h2_only");
    let ProtocolConfig::Foldfin(config) = ProtocolConfig::parse(&raw.to_string()).unwrap() else {
        unreachable!()
    };
    let dropped = Arc::new(AtomicUsize::new(0));
    let (device, _input, _output) = packets(dropped.clone());
    let manager = TunnelManager::default();
    let error = manager
        .start_foldfin_packets_on(&config, endpoint, device, Arc::new(LoopbackUnderlay))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            floppa_vpn_core::backend::BackendError::EndpointUnavailable { .. }
        ),
        "{error}"
    );
    assert_eq!(dropped.load(Ordering::SeqCst), 2);
    assert!(manager.meta().await.is_none());
}

#[tokio::test]
async fn authorization_failure_is_terminal_on_both_carriers() {
    for transport in ["h2_only", "auto"] {
        let (server_tls, ca) = tls();
        // No grants: the TLS identity is trusted, but authorization must fail.
        let server = Arc::new(DeviceRegistry::default())
            .server(server_tls)
            .unwrap();
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = tcp.local_addr().unwrap();
        let udp = std::net::UdpSocket::bind(endpoint).unwrap();
        let (stop, stopped) = oneshot::channel();
        let h3 = transport == "auto";
        let server_task = tokio::spawn(async move {
            if h3 {
                let result = server
                    .serve_h3(udp, async {
                        let _ = stopped.await;
                    })
                    .await;
                // Keep the TCP port bound but unaccepted for the entire H3 attempt.
                drop(tcp);
                result
            } else {
                server
                    .serve(tcp, async {
                        let _ = stopped.await;
                    })
                    .await
            }
        });
        let mut raw: serde_json::Value =
            serde_json::from_str(&document(&endpoint.to_string(), &ca)).unwrap();
        raw["config"]["transport"] = serde_json::json!(transport);
        let ProtocolConfig::Foldfin(config) = ProtocolConfig::parse(&raw.to_string()).unwrap()
        else {
            unreachable!()
        };
        let dropped = Arc::new(AtomicUsize::new(0));
        let (device, _input, _output) = packets(dropped.clone());
        let manager = TunnelManager::default();
        let failure = timeout(
            Duration::from_secs(5),
            manager.start_foldfin_packets_on(&config, endpoint, device, Arc::new(LoopbackUnderlay)),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(
            matches!(
                failure,
                floppa_vpn_core::backend::BackendError::ConnectionRejected { .. }
            ),
            "{failure}"
        );
        let failure = floppa_vpn_core::actor::types::AttemptError::Backend { error: failure };
        assert!(failure.is_fatal_for_cycle());
        assert!(!failure.permits_endpoint_retry());
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
        stop.send(()).unwrap();
        server_task.await.unwrap().unwrap();
    }
}
