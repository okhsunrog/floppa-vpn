fn main() {
    // On Windows, embed the app manifest for UAC elevation and copy wintun.dll
    #[cfg(windows)]
    {
        // Copy architecture-specific wintun.dll to root for bundling
        let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        let wintun_src = match target_arch.as_str() {
            "x86_64" => "third_party/x86_64/wintun.dll",
            "x86" => "third_party/i686/wintun.dll",
            "aarch64" => "third_party/aarch64/wintun.dll",
            _ => panic!("Unsupported Windows architecture: {}", target_arch),
        };

        let wintun_dest = "wintun.dll";
        if std::path::Path::new(wintun_src).exists() {
            std::fs::copy(wintun_src, wintun_dest)
                .expect("Failed to copy wintun.dll for bundling");
            println!("cargo:rerun-if-changed={}", wintun_src);
        } else {
            panic!(
                "wintun.dll not found at {}. Download from https://www.wintun.net/",
                wintun_src
            );
        }

        let mut windows = tauri_build::WindowsAttributes::new();
        windows = windows.app_manifest(include_str!("windows-app.manifest"));
        tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows))
            .expect("failed to run tauri-build");
    }

    #[cfg(not(windows))]
    tauri_build::build();
}
