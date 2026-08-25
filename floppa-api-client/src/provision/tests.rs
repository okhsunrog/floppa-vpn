//! The provisioning logic, driven by fakes.
//!
//! Each of these guards a distinction that the hand-written clients this replaces got wrong at
//! least once.

use super::*;
use crate::client::Refusal;
use crate::schema::{
    CreatePeerResponse, InstallationResponse, MeResponse, MyPeer, MySubscription, PeerSyncStatus,
    PublicConfig, SubscriptionSource, VlessConfigResponse,
};
use std::sync::Mutex;

/// What the fake server answers when asked about one protocol's peer: its id, "no such peer", or
/// nothing at all.
type PeerAnswer = Result<Option<i64>, ApiFailure>;

/// A server that answers exactly what it was told to, and records what it was asked.
#[derive(Default)]
struct FakeApi {
    subscribed: bool,
    me_unreachable: bool,
    amneziawg: bool,
    peers: Mutex<Vec<(Protocol, PeerAnswer)>>,
    /// What `sync_status` a found peer carries. `Active` unless a test says otherwise.
    peer_status: Option<PeerSyncStatus>,
    /// Consumed by the first `create_peer`; later ones succeed.
    create: Mutex<Option<Result<CreatePeerResponse, ApiFailure>>>,
    vless: Option<String>,
    created: Mutex<Vec<Protocol>>,
    looked_up: Mutex<Vec<Protocol>>,
}

impl FakeApi {
    fn answers(&self, protocol: Protocol) -> PeerAnswer {
        self.peers
            .lock()
            .unwrap()
            .iter()
            .find(|(p, _)| *p == protocol)
            .map(|(_, answer)| answer.clone())
            .unwrap_or(Ok(None))
    }
}

fn subscription() -> MySubscription {
    MySubscription {
        plan_name: "test".into(),
        plan_display_name: "Test".into(),
        source: SubscriptionSource::AdminGrant,
        starts_at: chrono::Utc::now(),
        expires_at: None,
        speed_limit_mbps: None,
        max_peers: 3,
    }
}

fn peer(id: i64, protocol: Protocol, status: PeerSyncStatus) -> MyPeer {
    MyPeer {
        id,
        assigned_ip: "10.0.0.2".into(),
        sync_status: status,
        protocol,
        download_bytes: 0,
        upload_bytes: 0,
        last_handshake: None,
        created_at: chrono::Utc::now(),
        device_name: None,
        device_id: Some("device-1".into()),
    }
}

#[async_trait]
impl ProvisionApi for FakeApi {
    async fn me(&self) -> Result<MeResponse, ApiFailure> {
        if self.me_unreachable {
            return Err(ApiFailure::Unreachable("no network".into()));
        }
        Ok(MeResponse {
            id: 1,
            is_admin: false,
            has_credential: false,
            telegram_linked: false,
            telegram_id: None,
            username: None,
            first_name: None,
            last_name: None,
            photo_url: None,
            subscription: self.subscribed.then(subscription),
        })
    }

    async fn public_config(&self) -> Result<PublicConfig, ApiFailure> {
        Ok(PublicConfig {
            amneziawg_available: self.amneziawg,
            vless_available: self.vless.is_some(),
            telegram_bot_username: None,
        })
    }

    async fn peer_by_device(
        &self,
        _device_id: &str,
        protocol: Protocol,
    ) -> Result<Option<MyPeer>, ApiFailure> {
        self.looked_up.lock().unwrap().push(protocol);
        self.answers(protocol).map(|found| {
            found.map(|id| {
                peer(
                    id,
                    protocol,
                    self.peer_status.unwrap_or(PeerSyncStatus::Active),
                )
            })
        })
    }

    async fn peer_config(&self, id: i64) -> Result<String, ApiFailure> {
        Ok(format!("[Interface]\n# peer {id}\n"))
    }

    async fn create_peer(&self, req: &CreatePeerRequest) -> Result<CreatePeerResponse, ApiFailure> {
        let protocol = req
            .protocol
            .expect("a peer is always created for a protocol");
        self.created.lock().unwrap().push(protocol);
        self.create
            .lock()
            .unwrap()
            .take()
            .unwrap_or(Ok(CreatePeerResponse {
                id: 1,
                assigned_ip: "10.0.0.3".into(),
                config: "[Interface]\n# fresh\n".into(),
            }))
    }

    async fn vless_config(&self) -> Result<VlessConfigResponse, ApiFailure> {
        match &self.vless {
            Some(uri) => Ok(VlessConfigResponse { uri: uri.clone() }),
            None => Err(ApiFailure::Refused(Refusal {
                status: 404,
                code: Some(ApiErrorCode::VlessNotConfigured),
                raw_code: "vless_not_configured".into(),
                message: "not configured".into(),
            })),
        }
    }

    async fn upsert_installation(
        &self,
        req: &UpsertInstallationRequest,
    ) -> Result<InstallationResponse, ApiFailure> {
        Ok(InstallationResponse {
            id: 1,
            device_id: req.device_id.clone(),
            device_name: req.device_name.clone(),
            platform: req.platform.clone(),
            app_version: req.app_version.clone(),
            created_at: chrono::Utc::now(),
            last_seen_at: chrono::Utc::now(),
        })
    }
}

#[derive(Default)]
struct FakeSink {
    imported: Mutex<Vec<String>>,
    refuse: bool,
}

#[async_trait]
impl ConfigSink for FakeSink {
    async fn import(&self, raw: String) -> Result<(), String> {
        if self.refuse {
            return Err("not a config this client understands".into());
        }
        self.imported.lock().unwrap().push(raw);
        Ok(())
    }

    async fn has_any(&self) -> bool {
        !self.imported.lock().unwrap().is_empty()
    }
}

fn identity() -> DeviceIdentity {
    DeviceIdentity {
        device_id: "device-1".into(),
        device_name: Some("test phone".into()),
        platform: "android".into(),
        app_version: "0.0.0".into(),
    }
}

fn refusal(status: u16, code: ApiErrorCode, raw: &str) -> ApiFailure {
    ApiFailure::Refused(Refusal {
        status,
        code: Some(code),
        raw_code: raw.into(),
        message: "refused".into(),
    })
}

// ---------------------------------------------------------------------------
// The distinction everything rests on
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_server_that_cannot_be_asked_never_reads_as_a_peer_that_is_gone() {
    let api = FakeApi {
        subscribed: true,
        peers: Mutex::new(vec![(
            Protocol::Wireguard,
            Err(ApiFailure::Unreachable("no network".into())),
        )]),
        ..Default::default()
    };
    let sink = FakeSink::default();

    let result =
        sync_wg_family_peer(&api, &sink, &identity(), true, Protocol::Wireguard, true).await;

    assert_eq!(result, SyncResult::Offline);
    // What this test exists for: nothing was created. Reading the failure as "no peer" is what
    // makes an offline start burn a slot from the account's peer limit on a duplicate.
    assert!(api.created.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_lookup_reports_the_three_answers_apart() {
    let api = FakeApi {
        peers: Mutex::new(vec![
            (Protocol::Wireguard, Ok(Some(7))),
            (
                Protocol::Amneziawg,
                Err(ApiFailure::Unreachable("no network".into())),
            ),
        ]),
        ..Default::default()
    };
    assert_eq!(
        lookup_peer(&api, "device-1", Protocol::Wireguard).await,
        PeerLookup::Found(7)
    );
    assert_eq!(
        lookup_peer(&api, "device-1", Protocol::Amneziawg).await,
        PeerLookup::Unknown
    );
    // Nothing recorded for this one, so the fake answers "no such peer".
    assert_eq!(
        lookup_peer(&api, "device-2", Protocol::Wireguard).await,
        PeerLookup::Found(7)
    );
}

// ---------------------------------------------------------------------------
// Syncing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_existing_peer_is_adopted_rather_than_replaced() {
    let api = FakeApi {
        subscribed: true,
        peers: Mutex::new(vec![(Protocol::Wireguard, Ok(Some(42)))]),
        ..Default::default()
    };
    let sink = FakeSink::default();

    let result =
        sync_wg_family_peer(&api, &sink, &identity(), true, Protocol::Wireguard, true).await;

    assert_eq!(result, SyncResult::Ok);
    assert!(api.created.lock().unwrap().is_empty());
    assert!(sink.imported.lock().unwrap()[0].contains("peer 42"));
}

#[tokio::test]
async fn a_missing_peer_is_created_only_when_creating_is_allowed() {
    let api = FakeApi {
        subscribed: true,
        ..Default::default()
    };
    let sink = FakeSink::default();

    // The secondary protocol on a server that does not offer it: looked up, never created.
    let result =
        sync_wg_family_peer(&api, &sink, &identity(), true, Protocol::Amneziawg, false).await;

    assert_eq!(result, SyncResult::Ok);
    assert!(api.created.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_refusal_keeps_the_reason_the_server_gave() {
    for (code, raw, expected) in [
        (
            ApiErrorCode::NoActiveSubscription,
            "no_active_subscription",
            SyncError::NoSubscription,
        ),
        (
            ApiErrorCode::PeerLimitReached,
            "peer_limit_reached",
            SyncError::PeerLimitReached,
        ),
    ] {
        let api = FakeApi {
            subscribed: true,
            create: Mutex::new(Some(Err(refusal(403, code, raw)))),
            ..Default::default()
        };
        let sink = FakeSink::default();

        let result =
            sync_wg_family_peer(&api, &sink, &identity(), true, Protocol::Wireguard, true).await;

        assert_eq!(result, SyncResult::Failed(expected));
    }
}

#[tokio::test]
async fn a_device_without_a_subscription_is_told_so_before_the_server_has_to_say_it() {
    let api = FakeApi::default();
    let sink = FakeSink::default();

    let result =
        sync_wg_family_peer(&api, &sink, &identity(), false, Protocol::Wireguard, true).await;

    assert_eq!(result, SyncResult::Failed(SyncError::NoSubscription));
    assert!(api.created.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_sync_stops_at_an_unreachable_server_rather_than_concluding_anything() {
    let api = FakeApi {
        me_unreachable: true,
        ..Default::default()
    };
    let sink = FakeSink::default();

    assert_eq!(
        sync_peers(&api, &sink, &identity()).await,
        SyncResult::Offline
    );
    assert!(api.looked_up.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_server_offering_amneziawg_provisions_both_and_prefers_it() {
    let api = FakeApi {
        subscribed: true,
        amneziawg: true,
        ..Default::default()
    };
    let sink = FakeSink::default();

    assert_eq!(sync_peers(&api, &sink, &identity()).await, SyncResult::Ok);
    assert_eq!(
        *api.created.lock().unwrap(),
        vec![Protocol::Amneziawg, Protocol::Wireguard]
    );
}

#[tokio::test]
async fn a_server_without_amneziawg_provisions_wireguard_and_only_wireguard() {
    let api = FakeApi {
        subscribed: true,
        amneziawg: false,
        ..Default::default()
    };
    let sink = FakeSink::default();

    assert_eq!(sync_peers(&api, &sink, &identity()).await, SyncResult::Ok);
    // The secondary is looked up and left alone: creating it would make a peer for a protocol
    // this server cannot carry.
    assert_eq!(*api.created.lock().unwrap(), vec![Protocol::Wireguard]);
}

#[tokio::test]
async fn a_bonus_peer_that_cannot_be_made_does_not_fail_the_sync() {
    let api = FakeApi {
        subscribed: true,
        amneziawg: true,
        // The primary already exists, so the one create this sync attempts is the secondary's.
        peers: Mutex::new(vec![(Protocol::Amneziawg, Ok(Some(5)))]),
        create: Mutex::new(Some(Err(refusal(
            403,
            ApiErrorCode::PeerLimitReached,
            "peer_limit_reached",
        )))),
        ..Default::default()
    };
    let sink = FakeSink::default();

    assert_eq!(sync_peers(&api, &sink, &identity()).await, SyncResult::Ok);
}

#[tokio::test]
async fn a_server_without_vless_is_not_a_failed_sync() {
    let api = FakeApi {
        subscribed: true,
        vless: None,
        ..Default::default()
    };
    let sink = FakeSink::default();

    assert_eq!(sync_peers(&api, &sink, &identity()).await, SyncResult::Ok);
    // Only the wg-family config landed.
    assert_eq!(sink.imported.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_vless_uri_is_stored_like_any_other_config() {
    let api = FakeApi {
        subscribed: true,
        vless: Some("vless://uuid@host:443?security=reality".into()),
        ..Default::default()
    };
    let sink = FakeSink::default();

    assert_eq!(sync_peers(&api, &sink, &identity()).await, SyncResult::Ok);
    assert!(
        sink.imported
            .lock()
            .unwrap()
            .iter()
            .any(|raw| raw.starts_with("vless://"))
    );
}

#[tokio::test]
async fn a_config_that_does_not_import_is_a_failure_and_not_a_silent_success() {
    let api = FakeApi {
        subscribed: true,
        peers: Mutex::new(vec![(Protocol::Wireguard, Ok(Some(3)))]),
        ..Default::default()
    };
    let sink = FakeSink {
        refuse: true,
        ..Default::default()
    };

    let result =
        sync_wg_family_peer(&api, &sink, &identity(), true, Protocol::Wireguard, true).await;

    assert!(matches!(
        result,
        SyncResult::Failed(SyncError::CreateFailed { .. })
    ));
}

// ---------------------------------------------------------------------------
// Repairing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_peer_that_is_still_there_is_left_alone() {
    let api = FakeApi {
        subscribed: true,
        peers: Mutex::new(vec![(Protocol::Amneziawg, Ok(Some(9)))]),
        ..Default::default()
    };
    let sink = FakeSink::default();

    assert_eq!(
        repair_peer(&api, &sink, &identity(), Protocol::Amneziawg).await,
        RepairOutcome::PeerExists
    );
    assert!(api.created.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_peer_that_is_gone_is_replaced() {
    let api = FakeApi {
        subscribed: true,
        amneziawg: true,
        ..Default::default()
    };
    let sink = FakeSink::default();

    assert_eq!(
        repair_peer(&api, &sink, &identity(), Protocol::Amneziawg).await,
        RepairOutcome::Recreated
    );
    assert!(api.created.lock().unwrap().contains(&Protocol::Amneziawg));
}

#[tokio::test]
async fn a_repair_that_cannot_reach_the_server_concludes_nothing() {
    let api = FakeApi {
        peers: Mutex::new(vec![(
            Protocol::Amneziawg,
            Err(ApiFailure::Unreachable("no network".into())),
        )]),
        ..Default::default()
    };
    let sink = FakeSink::default();

    assert_eq!(
        repair_peer(&api, &sink, &identity(), Protocol::Amneziawg).await,
        RepairOutcome::Unreachable
    );
    assert!(api.created.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_peer_on_its_way_out_reads_as_gone_rather_than_as_one_to_connect_over() {
    // The server has the row, and is in the middle of taking it off the interface. Adopting it
    // means building a tunnel over a peer that is about to stop answering.
    let api = FakeApi {
        subscribed: true,
        peers: Mutex::new(vec![(Protocol::Wireguard, Ok(Some(11)))]),
        peer_status: Some(PeerSyncStatus::PendingRemove),
        ..Default::default()
    };
    let sink = FakeSink::default();

    let result =
        sync_wg_family_peer(&api, &sink, &identity(), true, Protocol::Wireguard, true).await;

    assert_eq!(result, SyncResult::Ok);
    assert_eq!(*api.created.lock().unwrap(), vec![Protocol::Wireguard]);
}

#[tokio::test]
async fn a_peer_the_daemon_has_not_activated_yet_is_still_this_devices_peer() {
    // `pending_add` is a peer being created, not one missing: asking for a second would take
    // another slot from the account's limit for the same device.
    let api = FakeApi {
        subscribed: true,
        peers: Mutex::new(vec![(Protocol::Wireguard, Ok(Some(12)))]),
        peer_status: Some(PeerSyncStatus::PendingAdd),
        ..Default::default()
    };
    let sink = FakeSink::default();

    let result =
        sync_wg_family_peer(&api, &sink, &identity(), true, Protocol::Wireguard, true).await;

    assert_eq!(result, SyncResult::Ok);
    assert!(api.created.lock().unwrap().is_empty());
}
