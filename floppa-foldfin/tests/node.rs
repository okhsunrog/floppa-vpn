#![cfg(feature = "node")]
#[path = "../../floppa-core/tests/support/mod.rs"]
mod database;
mod support;
use floppa_core::{DbPool, foldfin as control};
use floppa_foldfin::{node::Node, tls::TlsIdentity};
use foldfin::{
    client::ClientConfig,
    credential::Credential,
    packet::{PacketDevice, PacketRecv, PacketSend},
    session::Carrier,
};
use std::{
    io,
    net::UdpSocket,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch},
    time::timeout,
};
struct Reader(mpsc::UnboundedReceiver<Vec<u8>>);
impl PacketRecv for Reader {
    fn poll_recv(&mut self, cx: &mut Context<'_>, buffer: &mut [u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(match std::task::ready!(self.0.poll_recv(cx)) {
            Some(p) => {
                buffer[..p.len()].copy_from_slice(&p);
                Ok(p.len())
            }
            None => Ok(0),
        })
    }
}
struct Writer(mpsc::UnboundedSender<Vec<u8>>);
impl PacketSend for Writer {
    fn poll_send(&mut self, _: &mut Context<'_>, packet: &[u8]) -> Poll<io::Result<()>> {
        Poll::Ready(self.0.send(packet.to_vec()).map_err(io::Error::other))
    }
}
fn device() -> (
    PacketDevice,
    mpsc::UnboundedSender<Vec<u8>>,
    mpsc::UnboundedReceiver<Vec<u8>>,
) {
    let (inject, recv) = mpsc::unbounded_channel();
    let (send, received) = mpsc::unbounded_channel();
    (
        PacketDevice::new(Reader(recv), Writer(send)),
        inject,
        received,
    )
}
fn packet(src: &str, dst: &str) -> Vec<u8> {
    let src: std::net::IpAddr = src.parse().unwrap();
    let dst: std::net::IpAddr = dst.parse().unwrap();
    match (src, dst) {
        (std::net::IpAddr::V4(s), std::net::IpAddr::V4(d)) => {
            let mut p = vec![0; 28];
            p[0] = 0x45;
            p[2..4].copy_from_slice(&28u16.to_be_bytes());
            p[8] = 64;
            p[9] = 17;
            p[12..16].copy_from_slice(&s.octets());
            p[16..20].copy_from_slice(&d.octets());
            p
        }
        (std::net::IpAddr::V6(s), std::net::IpAddr::V6(d)) => {
            let mut p = vec![0; 48];
            p[0] = 0x60;
            p[4..6].copy_from_slice(&8u16.to_be_bytes());
            p[6] = 17;
            p[7] = 64;
            p[8..24].copy_from_slice(&s.octets());
            p[24..40].copy_from_slice(&d.octets());
            p
        }
        _ => unreachable!(),
    }
}
fn vpn(slot: u32) -> foldfin::vpn::Config {
    foldfin::vpn::Config {
        local: database::config().addresses(slot),
        peer: database::config().addresses(1),
        local_routes: vec![],
        peer_routes: vec!["0.0.0.0/0".parse().unwrap(), "::/0".parse().unwrap()],
    }
}
async fn connect(
    h3: bool,
    address: std::net::SocketAddr,
    certificate: &[u8],
    token: &str,
) -> Carrier {
    let config = ClientConfig {
        authority: address.to_string(),
        server_name: "localhost".into(),
        credential: Credential::bearer(token).unwrap(),
        additional_ca_pem: Some(certificate.to_vec()),
    };
    if h3 {
        foldfin::h3_client::Client::connect_over(
            config,
            UdpSocket::bind("127.0.0.1:0").unwrap(),
            address,
        )
        .await
        .unwrap()
        .into()
    } else {
        foldfin::client::Client::connect_over(config, TcpStream::connect(address).await.unwrap())
            .await
            .unwrap()
            .into()
    }
}
async fn start(
    carrier: &Carrier,
    device: PacketDevice,
) -> foldfin::error::Result<foldfin::client::VpnTunnel> {
    start_slot(carrier, device, 2).await
}
async fn start_slot(
    carrier: &Carrier,
    device: PacketDevice,
    slot: u32,
) -> foldfin::error::Result<foldfin::client::VpnTunnel> {
    let tunnel = match carrier {
        Carrier::H2(c) => c.prepare_vpn(vpn(slot)).await?.start(device),
        Carrier::H3(c) => c.prepare_vpn(vpn(slot)).await?.start(device),
    };
    let mut states = tunnel.subscribe();
    timeout(Duration::from_secs(3), async {
        loop {
            match &*states.borrow_and_update() {
                foldfin::stats::SessionState::Running => return,
                foldfin::stats::SessionState::Failed { detail } => {
                    panic!("configuration failed: {detail}")
                }
                _ => (),
            }
            states.changed().await.unwrap();
        }
    })
    .await
    .expect("VPN configuration deadline");
    Ok(tunnel)
}
async fn acceptance(pool: DbPool, h3: bool) {
    timeout(Duration::from_secs(30), async {
        let (user, plan) = database::seed(&pool, 2).await;
        let (_, session) = database::device(&pool, user, "device").await;
        let session = session.parse().unwrap();
        let issued = control::issue(&pool, user, session, false).await.unwrap();
        let token = issued.config["token"].as_str().unwrap();
        let (_,second_session)=database::device(&pool,user,"second").await;
        let second=control::issue(&pool,user,second_session.parse().unwrap(),false).await.unwrap();
        let (cert, key) = support::certificate();
        let tls = Arc::new(TlsIdentity::from_pem(&cert, &key).unwrap());
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = tcp.local_addr().unwrap();
        let udp = UdpSocket::bind(address).unwrap();
        let (kernel, kernel_tx, mut kernel_rx) = device();
        let mut node = Node::new(
            pool.clone(),
            "singapore".into(),
            database::config(),
            Duration::from_secs(2),
            kernel,
        )
        .unwrap();
        node.synchronize().await.unwrap();
        assert_eq!(
            control::list(&pool, user).await.unwrap()[0].applied_revision,
            issued.revision
        );
        let (stop, mut stopped) = watch::channel(false);
        let running = tokio::spawn(node.serve(
            tls.server_config(),
            tcp,
            udp,
            Duration::from_millis(100),
            async move {
                let _ = stopped.wait_for(|v| *v).await;
            },
        ));
        let carrier = connect(h3, address, &cert, token).await;
        assert!(carrier.open_tcp("1.1.1.1:443").await.is_err());
        let (client, tx, mut rx) = device();
        let tunnel = start(&carrier, client).await.unwrap();
        for (local, remote) in [
            ("10.33.0.2", "1.1.1.1"),
            ("fd33::2", "2606:4700:4700::1111"),
        ] {
            let outbound = packet(local, remote);
            tx.send(outbound.clone()).unwrap();
            assert_eq!(
                timeout(Duration::from_secs(3), kernel_rx.recv())
                    .await
                    .expect("client to kernel")
                    .unwrap(),
                outbound
            );
            let inbound = packet(remote, local);
            kernel_tx.send(inbound.clone()).unwrap();
            assert_eq!(
                timeout(Duration::from_secs(3), rx.recv())
                    .await
                    .expect("kernel to client")
                    .unwrap(),
                inbound
            );
        }
        let other=connect(!h3,address,&cert,second.config["token"].as_str().unwrap()).await;
        let (client,other_tx,mut other_rx)=device();
        let other_tunnel=start_slot(&other,client,3).await.unwrap();
        let outgoing=packet("10.33.0.3","1.0.0.1");other_tx.send(outgoing.clone()).unwrap();
        assert_eq!(timeout(Duration::from_secs(3),kernel_rx.recv()).await.unwrap().unwrap(),outgoing);
        let incoming=packet("1.0.0.1","10.33.0.3");kernel_tx.send(incoming.clone()).unwrap();
        assert_eq!(timeout(Duration::from_secs(3),other_rx.recv()).await.unwrap().unwrap(),incoming);
        assert!(timeout(Duration::from_millis(50),rx.recv()).await.is_err());
        other_tx.send(packet("10.33.0.3","10.33.0.2")).unwrap();
        assert!(timeout(Duration::from_millis(50),kernel_rx.recv()).await.is_err());
        other_tunnel.stop().await.unwrap();other.stop().await.unwrap();
        let mut error = packet("10.33.0.1", "10.33.0.2");
        error[9] = 1;
        error[20] = 3;
        error[21] = 4;
        error.extend(packet("10.33.0.2", "1.1.1.1"));
        let len = error.len() as u16;
        error[2..4].copy_from_slice(&len.to_be_bytes());
        kernel_tx.send(error.clone()).unwrap();
        assert_eq!(
            timeout(Duration::from_secs(3), rx.recv())
                .await
                .expect("PMTU error")
                .unwrap(),
            error
        );
        for target in ["10.33.0.3", "10.44.0.2", "169.254.169.254"] {
            tx.send(packet("10.33.0.2", target)).unwrap();
        }
        assert!(
            timeout(Duration::from_millis(100), kernel_rx.recv())
                .await
                .is_err()
        );
        // Repeated reconciliation must renew the same lease without resetting a tunnel.
        tokio::time::sleep(Duration::from_millis(2200)).await;
        tx.send(packet("10.33.0.2", "1.1.1.1")).unwrap();
        assert!(
            timeout(Duration::from_secs(3), kernel_rx.recv())
                .await
                .expect("renewed tunnel")
                .is_some()
        );
        let rotated = control::issue(&pool, user, session, true).await.unwrap();
        timeout(Duration::from_secs(3), async {
            while control::list(&pool, user).await.unwrap()[0].applied_revision != rotated.revision
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(start(&carrier, device().0).await.is_err());
        timeout(Duration::from_secs(3), async {
            while rx.recv().await.is_some() {}
        })
        .await
        .unwrap();
        let _ = tunnel.stop().await;
        let fresh = connect(
            h3,
            address,
            &cert,
            rotated.config["token"].as_str().unwrap(),
        )
        .await;
        let (client, tx, mut rx) = device();
        let tunnel = start(&fresh, client).await.unwrap();
        // A broken complete snapshot must not renew access. Simulates failed DB reconciliation.
        sqlx::query("UPDATE foldfin_nodes SET config='{}'::jsonb WHERE region_id='singapore'")
            .execute(&pool)
            .await
            .unwrap();
        timeout(Duration::from_secs(4), async {
            while rx.recv().await.is_some() {}
        })
        .await
        .unwrap();
        assert!(start(&fresh, device().0).await.is_err());
        drop(tx);
        let _ = tunnel.stop().await;
        // Restore metadata; a new lease can be installed, the old tunnel stays closed.
        sqlx::query("UPDATE foldfin_nodes SET config=$1::jsonb WHERE region_id='singapore'")
            .bind(serde_json::to_string(&database::config()).unwrap())
            .execute(&pool)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let (client, _tx, mut rx) = device();
        let tunnel = start(&fresh, client).await.unwrap();
        // Removing the plan entitlement closes a running tunnel and rejects its token.
        sqlx::query("UPDATE plans SET foldfin_enabled=false WHERE id=(SELECT plan_id FROM current_subscriptions WHERE user_id=$1)")
            .bind(user).execute(&pool).await.unwrap();
        timeout(Duration::from_secs(3), async {
            while rx.recv().await.is_some() {}
        })
        .await
        .unwrap();
        assert!(start(&fresh, device().0).await.is_err());
        let _ = tunnel.stop().await;
        assert!(matches!(control::issue(&pool,user,session,true).await, Err(control::Error::Unavailable)));
        sqlx::query("UPDATE plans SET foldfin_enabled=true WHERE id=(SELECT plan_id FROM current_subscriptions WHERE user_id=$1)")
            .bind(user).execute(&pool).await.unwrap();
        // Subscription deadlines also close existing sessions, even between snapshots.
        let issued=control::issue(&pool,user,session,false).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let last=connect(h3,address,&cert,issued.config["token"].as_str().unwrap()).await;
        let (client,_tx,mut rx)=device();let tunnel=start(&last,client).await.unwrap();
        sqlx::query("UPDATE subscriptions SET expires_at=NOW()+INTERVAL '300 milliseconds' WHERE user_id=$1 AND is_current").bind(user).execute(&pool).await.unwrap();
        timeout(Duration::from_secs(3),async {while rx.recv().await.is_some() {}}).await.expect("subscription expiry");
        assert!(start(&last,device().0).await.is_err());let _=tunnel.stop().await;
        sqlx::query("UPDATE subscriptions SET expires_at=NULL WHERE user_id=$1 AND is_current").bind(user).execute(&pool).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let (client,_tx,mut rx)=device();let tunnel=start(&last,client).await.unwrap();
        // A downgrade must also be enforced by the node, even if the token is not revoked.
        let denied: i32 = sqlx::query_scalar("INSERT INTO plans(name,display_name) VALUES('no-foldfin','No Foldfin') RETURNING id").fetch_one(&pool).await.unwrap();
        sqlx::query("INSERT INTO plan_regions(plan_id,region_id) VALUES($1,'singapore')").bind(denied).execute(&pool).await.unwrap();
        sqlx::query("UPDATE subscriptions SET plan_id=$1 WHERE user_id=$2 AND is_current").bind(denied).bind(user).execute(&pool).await.unwrap();
        timeout(Duration::from_secs(3),async {while rx.recv().await.is_some() {}}).await.expect("plan downgrade");
        assert!(start(&last,device().0).await.is_err());let _=tunnel.stop().await;
        assert!(control::list(&pool,user).await.unwrap().iter().any(|d| d.id==issued.device_id && d.revoked_at.is_none()));
        sqlx::query("UPDATE subscriptions SET plan_id=$1 WHERE user_id=$2 AND is_current").bind(plan).bind(user).execute(&pool).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let (client,_tx,mut rx)=device();let tunnel=start(&last,client).await.unwrap();
        // Closing the node's shared pool makes every periodic acquisition fail.
        let closing=tokio::spawn({let pool=pool.clone();async move {pool.close().await}});
        timeout(Duration::from_secs(4),async {while rx.recv().await.is_some() {}}).await.expect("database outage lease deadline");
        assert!(start(&last,device().0).await.is_err());let _=tunnel.stop().await;
        last.stop().await.unwrap();
        carrier.stop().await.unwrap();
        fresh.stop().await.unwrap();
        stop.send_replace(true);
        running.await.unwrap().unwrap();
        closing.await.unwrap();
        assert!(kernel_tx.send(packet("1.1.1.1", "10.33.0.2")).is_err());
    })
    .await
    .unwrap();
}
#[sqlx::test(migrations = "../migrations")]
async fn h2_control_plane_lifecycle(pool: DbPool) {
    acceptance(pool, false).await;
}
#[sqlx::test(migrations = "../migrations")]
async fn h3_control_plane_lifecycle(pool: DbPool) {
    acceptance(pool, true).await;
}

#[sqlx::test(migrations = "../migrations")]
async fn packet_device_failure_closes_both_listeners(pool: DbPool) {
    database::seed(&pool, 1).await;
    let (cert, key) = support::certificate();
    let tls = Arc::new(TlsIdentity::from_pem(&cert, &key).unwrap());
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = tcp.local_addr().unwrap();
    let udp = UdpSocket::bind(address).unwrap();
    let (kernel, inject, _receive) = device();
    drop(inject);
    let node = Node::new(
        pool,
        "singapore".into(),
        database::config(),
        Duration::from_secs(2),
        kernel,
    )
    .unwrap();
    let result = timeout(
        Duration::from_secs(5),
        node.serve(
            tls.server_config(),
            tcp,
            udp,
            Duration::from_millis(100),
            std::future::pending(),
        ),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    assert!(TcpStream::connect(address).await.is_err());
    UdpSocket::bind(address).unwrap();
}
