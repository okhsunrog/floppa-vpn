//! The provisioning logic, driven by fakes.
//!
//! Every one of these was a bug at some point in the frontend implementation this replaces, or
//! guards the distinction that made those bugs possible.

use super::*;
use crate::vpn::actor::types::AttemptFailure;
use api::{CreatePeerResponse, MeResponse, MyPeer, PublicConfig, Refusal, VlessConfigResponse};
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
    /// Per protocol: what `peer_by_device` answers.
    peers: Mutex<Vec<(WgProtocol, PeerAnswer)>>,
    create: Mutex<Option<Result<CreatePeerResponse, ApiFailure>>>,
    vless: Option<Result<String, ApiFailure>>,
    created: Mutex<Vec<WgProtocol>>,
    looked_up: Mutex<Vec<WgProtocol>>,
}

impl FakeApi {
    fn answers(&self, protocol: WgProtocol) -> PeerAnswer {
        self.peers
            .lock()
            .unwrap()
            .iter()
            .find(|(p, _)| *p == protocol)
            .map(|(_, answer)| answer.clone())
            .unwrap_or(Ok(None))
    }
}

#[async_trait]
impl ProvisionApi for FakeApi {
    async fn me(&self) -> Result<MeResponse, ApiFailure> {
        if self.me_unreachable {
            return Err(ApiFailure::Unreachable("no network".into()));
        }
        Ok(MeResponse {
            subscription: self.subscribed.then(|| serde_json::json!({"plan": "test"})),
        })
    }

    async fn public_config(&self) -> Result<PublicConfig, ApiFailure> {
        Ok(PublicConfig {
            amneziawg_available: self.amneziawg,
            vless_available: false,
        })
    }

    async fn peer_by_device(
        &self,
        _device_id: &str,
        protocol: WgProtocol,
    ) -> Result<Option<MyPeer>, ApiFailure> {
        self.looked_up.lock().unwrap().push(protocol);
        self.answers(protocol)
            .map(|found| found.map(|id| MyPeer { id }))
    }

    async fn peer_config(&self, id: i64) -> Result<String, ApiFailure> {
        Ok(format!("[Interface]\n# peer {id}\n"))
    }

    async fn create_peer(&self, req: &CreatePeerRequest) -> Result<CreatePeerResponse, ApiFailure> {
        self.created.lock().unwrap().push(req.protocol);
        self.create
            .lock()
            .unwrap()
            .take()
            .unwrap_or(Ok(CreatePeerResponse {
                id: 1,
                config: "[Interface]\n# fresh\n".into(),
            }))
    }

    async fn vless_config(&self) -> Result<VlessConfigResponse, ApiFailure> {
        match &self.vless {
            Some(Ok(uri)) => Ok(VlessConfigResponse { uri: uri.clone() }),
            Some(Err(e)) => Err(e.clone()),
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
        _req: &UpsertInstallationRequest,
    ) -> Result<(), ApiFailure> {
        Ok(())
    }
}

#[derive(Default)]
struct FakeSink {
    imported: Mutex<Vec<String>>,
    refuse: bool,
}

#[async_trait]
impl ConfigSink for FakeSink {
    async fn import(&self, raw: String) -> Result<Protocol, String> {
        if self.refuse {
            return Err("not a config this client understands".into());
        }
        self.imported.lock().unwrap().push(raw);
        Ok(Protocol::WireGuard)
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
            WgProtocol::Wireguard,
            Err(ApiFailure::Unreachable("no network".into())),
        )]),
        ..Default::default()
    };
    let sink = FakeSink::default();

    let result =
        sync_wg_family_peer(&api, &sink, &identity(), true, WgProtocol::Wireguard, true).await;

    assert_eq!(result, SyncResult::Offline);
    // The thing this test exists for: nothing was created. Reading the failure as "no peer" is
    // what used to make an offline start burn a peer slot on a duplicate.
    assert!(api.created.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_lookup_reports_the_three_answers_apart() {
    let api = FakeApi {
        peers: Mutex::new(vec![
            (WgProtocol::Wireguard, Ok(Some(7))),
            (
                WgProtocol::Amneziawg,
                Err(ApiFailure::Unreachable("x".into())),
            ),
        ]),
        ..Default::default()
    };
    assert_eq!(
        lookup_peer(&api, "device-1", WgProtocol::Wireguard).await,
        PeerLookup::Found(7)
    );
    assert_eq!(
        lookup_peer(&api, "device-1", WgProtocol::Amneziawg).await,
        PeerLookup::Unknown
    );
}

// ---------------------------------------------------------------------------
// Syncing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_existing_peer_is_adopted_rather_than_replaced() {
    let api = FakeApi {
        subscribed: true,
        peers: Mutex::new(vec![(WgProtocol::Wireguard, Ok(Some(42)))]),
        ..Default::default()
    };
    let sink = FakeSink::default();

    let result =
        sync_wg_family_peer(&api, &sink, &identity(), true, WgProtocol::Wireguard, true).await;

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
        sync_wg_family_peer(&api, &sink, &identity(), true, WgProtocol::Amneziawg, false).await;

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
            sync_wg_family_peer(&api, &sink, &identity(), true, WgProtocol::Wireguard, true).await;

        assert_eq!(result, SyncResult::Failed { error: expected });
    }
}

#[tokio::test]
async fn a_device_without_a_subscription_is_told_so_before_the_server_has_to_say_it() {
    let api = FakeApi::default();
    let sink = FakeSink::default();

    let result =
        sync_wg_family_peer(&api, &sink, &identity(), false, WgProtocol::Wireguard, true).await;

    assert_eq!(
        result,
        SyncResult::Failed {
            error: SyncError::NoSubscription
        }
    );
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

    let created = api.created.lock().unwrap().clone();
    assert_eq!(created, vec![WgProtocol::Amneziawg, WgProtocol::Wireguard]);
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
    assert_eq!(*api.created.lock().unwrap(), vec![WgProtocol::Wireguard]);
}

#[tokio::test]
async fn a_bonus_peer_that_cannot_be_made_does_not_fail_the_sync() {
    let api = FakeApi {
        subscribed: true,
        amneziawg: true,
        peers: Mutex::new(vec![(WgProtocol::Amneziawg, Ok(Some(5)))]),
        // The one create call this sync makes is the secondary's, and it is refused.
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
async fn a_config_that_does_not_import_is_a_failure_and_not_a_silent_success() {
    let api = FakeApi {
        subscribed: true,
        peers: Mutex::new(vec![(WgProtocol::Wireguard, Ok(Some(3)))]),
        ..Default::default()
    };
    let sink = FakeSink {
        refuse: true,
        ..Default::default()
    };

    let result =
        sync_wg_family_peer(&api, &sink, &identity(), true, WgProtocol::Wireguard, true).await;

    assert!(matches!(
        result,
        SyncResult::Failed {
            error: SyncError::CreateFailed { .. }
        }
    ));
}

// ---------------------------------------------------------------------------
// Planning what a finished cycle means
// ---------------------------------------------------------------------------

fn failure(protocol: Protocol, error: AttemptError) -> AttemptFailure {
    AttemptFailure {
        protocol,
        error,
        pass: 0,
    }
}

#[test]
fn a_cycle_that_connected_over_a_fallback_owes_the_one_it_stepped_over_a_peer() {
    let outcome = CycleOutcome::Connected {
        protocol: Protocol::WireGuard,
        adopted: false,
        failures: vec![failure(Protocol::AmneziaWg, AttemptError::VerifyFailed)],
    };
    assert_eq!(
        plan_outcome(&outcome),
        OutcomePlan::Repair {
            protocol: WgProtocol::Amneziawg
        }
    );
}

#[test]
fn a_cycle_that_connected_cleanly_owes_nothing() {
    let outcome = CycleOutcome::Connected {
        protocol: Protocol::AmneziaWg,
        adopted: false,
        failures: vec![],
    };
    assert_eq!(plan_outcome(&outcome), OutcomePlan::Ignore);
}

#[test]
fn the_protocol_that_failed_verification_is_found_by_name_not_by_position() {
    // WireGuard was tried last and timed out; AmneziaWG is the one whose peer may be gone.
    let outcome = CycleOutcome::Exhausted {
        failures: vec![
            failure(Protocol::AmneziaWg, AttemptError::VerifyFailed),
            failure(Protocol::WireGuard, AttemptError::TimedOut),
        ],
    };
    assert_eq!(
        plan_outcome(&outcome),
        OutcomePlan::Reprovision {
            protocol: WgProtocol::Amneziawg
        }
    );
}

#[test]
fn a_failure_no_new_peer_would_fix_asks_for_no_new_peer() {
    let outcome = CycleOutcome::Exhausted {
        failures: vec![failure(
            Protocol::WireGuard,
            AttemptError::ResolveFailed {
                host: "vpn.example".into(),
                detail: "no DNS".into(),
            },
        )],
    };
    assert_eq!(plan_outcome(&outcome), OutcomePlan::Ignore);
}

#[test]
fn vless_never_asks_for_a_peer_because_it_has_none() {
    let outcome = CycleOutcome::Exhausted {
        failures: vec![failure(Protocol::Vless, AttemptError::VerifyFailed)],
    };
    assert_eq!(plan_outcome(&outcome), OutcomePlan::Ignore);

    let lost = CycleOutcome::LostGaveUp {
        protocol: Protocol::Vless,
        passes: 3,
    };
    assert_eq!(plan_outcome(&lost), OutcomePlan::Ignore);
}

#[test]
fn the_endings_that_are_nobodys_fault_ask_for_nothing() {
    for outcome in [
        CycleOutcome::Cancelled,
        CycleOutcome::Down,
        CycleOutcome::UnwindFailed,
    ] {
        assert_eq!(plan_outcome(&outcome), OutcomePlan::Ignore);
    }
}

// ---------------------------------------------------------------------------
// Repairing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_peer_that_is_still_there_is_left_alone() {
    let api = FakeApi {
        subscribed: true,
        peers: Mutex::new(vec![(WgProtocol::Amneziawg, Ok(Some(9)))]),
        ..Default::default()
    };
    let sink = FakeSink::default();

    assert_eq!(
        repair_peer(&api, &sink, &identity(), WgProtocol::Amneziawg).await,
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
        repair_peer(&api, &sink, &identity(), WgProtocol::Amneziawg).await,
        RepairOutcome::Recreated
    );
    assert!(api.created.lock().unwrap().contains(&WgProtocol::Amneziawg));
}

#[tokio::test]
async fn a_repair_that_cannot_reach_the_server_concludes_nothing() {
    let api = FakeApi {
        peers: Mutex::new(vec![(
            WgProtocol::Amneziawg,
            Err(ApiFailure::Unreachable("no network".into())),
        )]),
        ..Default::default()
    };
    let sink = FakeSink::default();

    assert_eq!(
        repair_peer(&api, &sink, &identity(), WgProtocol::Amneziawg).await,
        RepairOutcome::Unreachable
    );
    assert!(api.created.lock().unwrap().is_empty());
}
