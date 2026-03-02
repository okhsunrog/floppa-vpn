use std::process::Command;

fn main() {
    memory_serve::load_directory("../../floppa-face/dist");

    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=Cargo.toml");
    println!("cargo::rerun-if-changed=../../.git/HEAD");
    println!("cargo::rerun-if-changed=../../.git/refs/heads/");

    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let build_time = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();

    println!("cargo:rustc-env=GIT_HASH={git_hash}");
    println!("cargo:rustc-env=BUILD_TIME={build_time}");
}
