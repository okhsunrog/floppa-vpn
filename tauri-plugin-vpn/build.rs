const COMMANDS: &[&str] = &[
    "prepare_vpn",
    "start_vpn",
    "stop_vpn",
    "get_vpn_status",
    "get_installed_apps",
    "protect_socket",
    "get_safe_area_insets",
    "register_listener",
    "is_battery_optimization_disabled",
    "request_disable_battery_optimization",
    "are_notifications_enabled",
    "open_notification_settings",
    "get_device_name",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .android_path("android")
        .build();
}
