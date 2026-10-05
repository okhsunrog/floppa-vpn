use std::process::Command;

fn main() {
    memory_serve::load_directory("../floppa-face/dist");

    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=Cargo.toml");
    // A worktree's .git is a file; ask Git for the actual ref paths.
    for git_path in ["HEAD", "refs/heads", "packed-refs"] {
        if let Ok(output) = Command::new("git")
            .args(["rev-parse", "--git-path", git_path])
            .output()
            && output.status.success()
            && let Ok(path) = String::from_utf8(output.stdout)
        {
            println!("cargo::rerun-if-changed={}", path.trim());
        }
    }

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
