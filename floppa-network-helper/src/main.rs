//! Privileged, synchronous Linux network helper. No VPN engine or UI code runs here.
#[cfg(target_os = "linux")]
mod command;
#[cfg(target_os = "linux")]
mod dns;
#[cfg(target_os = "linux")]
mod journal;
#[cfg(target_os = "linux")]
mod network;
#[cfg(target_os = "linux")]
mod policy;
#[cfg(target_os = "linux")]
mod validate;
#[cfg(target_os = "linux")]
use anyhow::{Context as _, Result, bail, ensure};

#[cfg(target_os = "linux")]
struct Context {
    journal: journal::Journal,
    cmd: command::Commands,
    boot: String,
}
#[cfg(target_os = "linux")]
fn new_id() -> Result<String> {
    let id = std::fs::read_to_string("/proc/sys/kernel/random/uuid")?
        .trim()
        .to_string();
    validate::boot(&id)?;
    Ok(id)
}
#[cfg(target_os = "linux")]
fn checkpoint(_name: &str) {
    #[cfg(feature = "test-hooks")]
    {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let count = COUNT.fetch_add(1, Ordering::SeqCst) + 1;
        if let Ok(target) = std::env::var("FLOPPA_TEST_CRASH")
            && (target == _name || target == count.to_string())
        {
            // SAFETY: kill this test process, leaving real kernel/filesystem state behind.
            unsafe {
                libc::kill(libc::getpid(), libc::SIGKILL);
            }
        }
    }
}
#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("floppa-network-helper is only supported on Linux");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["protocol-version"] {
        println!("2");
        return Ok(());
    }
    ensure!(
        args.len() <= 65536 && args.iter().all(|s| s.len() <= 256),
        "oversized command"
    );
    // SAFETY: geteuid has no preconditions.
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "network helper requires root (run through pkexec)"
    );
    #[cfg(feature = "test-hooks")]
    for namespace in ["net", "mnt"] {
        ensure!(
            std::fs::read_link(format!("/proc/self/ns/{namespace}"))?
                != std::fs::read_link(format!("/proc/1/ns/{namespace}"))?,
            "test helper refuses host namespaces"
        );
    }
    if args == ["install"] {
        return install();
    }
    let command = parse(args)?;
    // Every operation, including preflight and link configuration, shares one stable lock.
    let journal = journal::Journal::open()?;
    let cmd = command::Commands {
        lock_fd: journal.fd(),
    };
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_string();
    validate::boot(&boot)?;
    let ctx = Context { journal, cmd, boot };
    match command {
        Operation::Check => ctx.check_policy(),
        Operation::Ownership => prepare_ownership(),
        Operation::Recover(name) => match name.as_str() {
            "dns" => ctx.recover_dns(),
            "policy" => ctx.recover_policy(),
            "routes" => ctx.recover_routes(),
            "link" => ctx.recover_link(),
            "network" => {
                // One failed undo must not strand the unrelated components. Link deletion
                // waits for its dependencies, and every failed record remains retryable.
                let mut errors = Vec::new();
                for (name, result) in [
                    ("dns", ctx.recover_dns()),
                    ("policy", ctx.recover_policy()),
                    ("routes", ctx.recover_routes()),
                    ("link", ctx.recover_link()),
                ] {
                    if let Err(e) = result {
                        errors.push(format!("{name}: {e:#}"));
                    }
                }
                ensure!(
                    errors.is_empty(),
                    "incomplete network recovery: {}",
                    errors.join("; ")
                );
                Ok(())
            }
            _ => unreachable!(),
        },
        Operation::Tun(iface, uid) => ctx.ensure_tun(&iface, uid),
        Operation::Mtu(iface, mtu) => {
            ctx.cmd
                .ip(&["link", "set", "dev", &iface, "mtu", &mtu.to_string()])
        }
        Operation::Address(iface, addr, remove) => {
            if remove {
                ctx.flush_address(&iface, &addr)
            } else {
                ctx.cmd.ip(&["addr", "replace", &addr, "dev", &iface])?;
                ctx.cmd.ip(&["link", "set", &iface, "up"])
            }
        }
        Operation::Routes(iface, dest) => ctx.add_routes(&iface, &dest),
        Operation::Policy(v4, v6) => ctx.add_policy(v4, v6),
        Operation::Dns(iface, servers, auto) => {
            if auto {
                ctx.set_dns_auto(&iface, &servers)
            } else {
                ctx.set_dns(&iface, &servers)
            }
        }
        Operation::FileDns(servers) => ctx.set_dns_file(&servers),
    }
}
#[cfg(target_os = "linux")]
enum Operation {
    Check,
    Ownership,
    Recover(String),
    Tun(String, u32),
    Mtu(String, u16),
    Address(String, String, bool),
    Routes(String, Vec<String>),
    Policy(bool, bool),
    Dns(String, Vec<String>, bool),
    FileDns(Vec<String>),
}
#[cfg(target_os = "linux")]
fn parse(args: Vec<String>) -> Result<Operation> {
    let strings: Vec<&str> = args.iter().map(String::as_str).collect();
    Ok(match strings.as_slice() {
        ["check-policy"] => Operation::Check,
        ["prepare-ownership"] => Operation::Ownership,
        [name]
            if [
                "recover-network",
                "recover-dns",
                "recover-policy",
                "recover-routes",
                "recover-link",
            ]
            .contains(name) =>
        {
            Operation::Recover(name.strip_prefix("recover-").unwrap().into())
        }
        ["ensure-tun", iface, uid] => {
            validate::iface(iface)?;
            Operation::Tun(iface.to_string(), uid.parse().context("invalid UID")?)
        }
        ["set-mtu", iface, mtu] => {
            validate::iface(iface)?;
            let mtu = mtu.parse()?;
            ensure!((1280..=9000).contains(&mtu), "invalid MTU");
            Operation::Mtu(iface.to_string(), mtu)
        }
        [name @ ("configure" | "flush-addr"), iface, addr] => {
            validate::iface(iface)?;
            validate::network(addr)?;
            Operation::Address(iface.to_string(), addr.to_string(), *name == "flush-addr")
        }
        ["add-routes", iface, rest @ ..] if !rest.is_empty() => {
            validate::iface(iface)?;
            for s in rest {
                validate::network(s)?;
            }
            Operation::Routes(
                iface.to_string(),
                rest.iter().map(|s| s.to_string()).collect(),
            )
        }
        ["add-policy", v4 @ ("0" | "1"), v6 @ ("0" | "1")] => {
            Operation::Policy(*v4 == "1", *v6 == "1")
        }
        [name @ ("set-dns" | "set-dns-auto"), iface, rest @ ..] if !rest.is_empty() => {
            validate::iface(iface)?;
            Operation::Dns(iface.to_string(), servers(rest)?, *name == "set-dns-auto")
        }
        ["set-dns-file", rest @ ..] if !rest.is_empty() => Operation::FileDns(servers(rest)?),
        _ => bail!("invalid helper command or arguments"),
    })
}
#[cfg(target_os = "linux")]
fn servers(values: &[&str]) -> Result<Vec<String>> {
    values
        .iter()
        .map(|s| {
            s.parse::<std::net::IpAddr>()
                .map(|v| v.to_string())
                .context("invalid DNS server IP")
        })
        .collect()
}
#[cfg(target_os = "linux")]
fn prepare_ownership() -> Result<()> {
    use std::{
        fs::{OpenOptions, Permissions},
        os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        path::Path,
    };
    journal::secure_dir(Path::new("/run/floppa-vpn"), 0o755)?;
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o444)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open("/run/floppa-vpn/tunnel.lock")?;
    let m = f.metadata()?;
    ensure!(
        m.is_file() && m.uid() == 0 && m.nlink() == 1,
        "unsafe tunnel ownership lock"
    );
    f.set_permissions(Permissions::from_mode(0o444))?;
    f.sync_all()?;
    journal::sync_dir(Path::new("/run/floppa-vpn"))
}
#[cfg(target_os = "linux")]
fn install() -> Result<()> {
    use std::path::Path;
    journal::secure_dir(Path::new("/usr/lib/floppa-vpn"), 0o755)?;
    let bytes = std::fs::read(std::env::current_exe()?)?;
    journal::atomic_write(
        Path::new("/usr/lib/floppa-vpn/floppa-network-helper"),
        &bytes,
        0o755,
    )?;
    journal::atomic_write(
        Path::new("/usr/share/polkit-1/actions/dev.okhsunrog.floppa-vpn.policy"),
        include_bytes!("../../floppa-vpn-core/resources/linux/dev.okhsunrog.floppa-vpn.policy"),
        0o644,
    )
}
