use floppa_tunnel_config::route::{exclude_networks, split_default, with_extra_routes};
use ipnetwork::IpNetwork;
fn net(s: &str) -> IpNetwork {
    s.parse().unwrap()
}

#[test]
fn exclusion_wins_over_overlapping_server_grants() {
    let original = split_default(&[net("0.0.0.0/0")], false);
    let routes = exclude_networks(
        with_extra_routes(original, &[net("194.186.63.128/25")]),
        &[net("194.186.63.0/24")],
    );
    for address in ["194.186.63.0", "194.186.63.1", "194.186.63.255"] {
        assert!(!routes.iter().any(|r| r.contains(address.parse().unwrap())));
    }
    for address in [
        "0.0.0.0",
        "194.186.62.255",
        "194.186.64.0",
        "255.255.255.255",
    ] {
        assert!(routes.iter().any(|r| r.contains(address.parse().unwrap())));
    }
    let count: u64 = routes.iter().map(|r| 1u64 << (32 - r.prefix())).sum();
    assert_eq!(count, (1u64 << 32) - 256);
}

#[test]
fn empty_nested_and_full_exclusions() {
    let routes = split_default(&[net("0.0.0.0/0")], false);
    assert_eq!(exclude_networks(routes.clone(), &[]), routes);
    assert!(exclude_networks(routes.clone(), &[net("0.0.0.0/0")]).is_empty());
    assert_eq!(
        exclude_networks(routes.clone(), &[net("10.0.0.0/8"), net("10.1.0.0/16")]),
        exclude_networks(routes, &[net("10.0.0.0/8")])
    );
    assert_eq!(
        exclude_networks(vec![net("10.0.0.0/8")], &[net("192.0.2.0/24")]),
        vec![net("10.0.0.0/8")]
    );
}

#[test]
#[ignore = "requires sudo and network namespaces; never changes host routes"]
fn linux_network_handover() {
    let routes = exclude_networks(
        split_default(&[net("0.0.0.0/0")], false),
        &[net("194.186.63.0/24")],
    );
    let result = std::process::Command::new("sudo")
        .args(["-n", "unshare", "--mount", "--net", "env"])
        .arg(format!(
            "FLOPPA_TEST_PATH={}",
            std::env::var("PATH").unwrap()
        ))
        .arg("bash")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tests/routing/exclusions.sh"
        ))
        .args(routes.iter().map(ToString::to_string))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
