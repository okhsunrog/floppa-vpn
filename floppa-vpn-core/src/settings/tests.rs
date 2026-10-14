use super::*;
use crate::protocol::Protocol::{AmneziaWg, Vless, WireGuard};

fn store(path: &std::path::Path) -> SettingsStore {
    SettingsStore::new(
        path.to_path_buf(),
        SettingsCapabilities {
            destination_exclusions: true,
            app_routing: true,
            connect_on_boot: true,
        },
    )
}

#[test]
fn save_reload_and_revision_conflict_do_not_lose_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let mut disk = store(&path);
    assert_eq!(disk.read().unwrap(), None);
    assert!(!path.exists());
    let mut settings = Settings::default();
    settings.protocols.auto_order = vec![AmneziaWg, Vless];
    settings.routing.excluded_routes = vec!["194.186.63.0/24".into()];
    let saved = disk.replace(0, settings.clone()).unwrap();
    assert_eq!(saved.revision, 1);
    assert_eq!(store(&path).read().unwrap(), Some(saved.clone()));
    assert!(matches!(
        disk.replace(0, Settings::default()),
        Err(SettingsError::Conflict { actual: 1, .. })
    ));
    assert_eq!(disk.read().unwrap(), Some(saved));
    settings.startup.connect_on_boot = true;
    assert_eq!(disk.replace(1, settings).unwrap().revision, 2);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn broken_unknown_and_invalid_documents_are_never_replaced_with_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let mut disk = store(&path);
    let valid = serde_json::to_value(SettingsDocument {
        schema_version: 1,
        revision: 1,
        settings: Settings::default(),
    })
    .unwrap();
    let mut future = valid.clone();
    future["schema_version"] = 999.into();
    let mut typo = valid.clone();
    typo["protcols"] = serde_json::json!({});
    let mut empty = valid.clone();
    empty["protocols"]["auto_order"] = serde_json::json!([]);
    let mut duplicate = valid.clone();
    duplicate["protocols"]["auto_order"] = serde_json::json!(["vless", "vless"]);
    let mut rounds = valid.clone();
    rounds["recovery"] = serde_json::json!({"mode":"limited", "rounds":0});
    let mut revision = valid;
    revision["revision"] = 0.into();
    for bytes in [
        b"{".to_vec(),
        serde_json::to_vec(&future).unwrap(),
        serde_json::to_vec(&typo).unwrap(),
        serde_json::to_vec(&empty).unwrap(),
        serde_json::to_vec(&duplicate).unwrap(),
        serde_json::to_vec(&rounds).unwrap(),
        serde_json::to_vec(&revision).unwrap(),
    ] {
        std::fs::write(&path, &bytes).unwrap();
        assert!(disk.read().is_err());
        assert!(disk.replace(1, Settings::default()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn failed_write_does_not_publish_a_new_revision() {
    let dir = tempfile::tempdir().unwrap();
    let mut disk = store(&dir.path().join("missing/settings.json"));
    assert!(matches!(
        disk.replace(0, Settings::default()),
        Err(SettingsError::Io(_))
    ));
    assert_eq!(disk.read().unwrap(), None);
}

#[test]
fn capabilities_are_checked_without_silently_dropping_fields() {
    let mut settings = Settings::default();
    settings.startup.connect_on_boot = true;
    assert!(matches!(
        settings.validate(SettingsCapabilities::default()),
        Err(SettingsError::Unsupported("connect on boot"))
    ));
    settings.startup.connect_on_boot = false;
    settings.routing.excluded_routes = vec!["194.186.63.0/24".into()];
    assert!(matches!(
        settings.validate(SettingsCapabilities::default()),
        Err(SettingsError::Unsupported("destination exclusions"))
    ));
}

#[test]
fn reconnect_prefers_last_success_only_inside_the_allowlist() {
    let settings = ProtocolSettings {
        auto_order: vec![AmneziaWg, Vless],
        ..ProtocolSettings::default()
    };
    assert_eq!(
        settings.plan(&Protocol::ALL, ConnectReason::User).unwrap(),
        [AmneziaWg, Vless]
    );
    assert_eq!(
        settings
            .plan(
                &Protocol::ALL,
                ConnectReason::Recovery {
                    last_successful: Some(Vless)
                }
            )
            .unwrap(),
        [Vless, AmneziaWg]
    );
    assert_eq!(
        settings
            .plan(
                &Protocol::ALL,
                ConnectReason::Recovery {
                    last_successful: Some(WireGuard)
                }
            )
            .unwrap(),
        [AmneziaWg, Vless]
    );
    assert_eq!(settings.auto_order, [AmneziaWg, Vless]);
    assert!(matches!(
        settings.plan(&[WireGuard], ConnectReason::User),
        Err(SettingsError::NoAvailableProtocol)
    ));
    let manual = ProtocolSettings {
        mode: SelectionMode::Manual,
        manual: AmneziaWg,
        ..settings
    };
    assert!(manual.plan(&[Vless], ConnectReason::User).is_err());
    assert_eq!(
        manual
            .plan(
                &Protocol::ALL,
                ConnectReason::Recovery {
                    last_successful: Some(Vless)
                }
            )
            .unwrap(),
        [AmneziaWg]
    );
}
