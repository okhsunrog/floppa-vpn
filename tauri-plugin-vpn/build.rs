const COMMANDS: &[&str] = &[
    "prepare_vpn",
    "start_vpn",
    "stop_vpn",
    "get_vpn_status",
    "get_installed_apps",
    "protect_socket",
    "get_safe_area_insets",
    "register_listener",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .android_path("android")
        .build();
}
