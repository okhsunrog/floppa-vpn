use crate::{
    Context as Helper,
    journal::Record,
    policy::{TABLE, reserved_route},
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
impl Helper {
    pub fn link(&self, name: &str) -> Result<Option<Value>> {
        // Query the complete list: an unsuccessful query is never interpreted as absence.
        let links = self.cmd.json("ip", &["-j", "-d", "link", "show"])?;
        let links = links.as_array().context("invalid link array")?;
        Ok(links.iter().find(|v| v["ifname"] == name).cloned())
    }
    pub fn index(&self, name: &str) -> Result<u32> {
        let n = self.link(name)?.context("missing tunnel link")?["ifindex"]
            .as_u64()
            .context("missing interface index")?;
        Ok(n.try_into()?)
    }
    pub fn ensure_tun(&self, iface: &str, uid: u32) -> Result<()> {
        ensure!(
            self.link(iface)?.is_none(),
            "{iface} already exists; recover the previous tunnel first"
        );
        let token = crate::new_id()?;
        let temporary = format!("flpt{}", &token[..8]);
        ensure!(
            self.link(&temporary)?.is_none(),
            "temporary tunnel name collision"
        );
        self.journal.save(
            "link",
            Record::Link {
                boot: self.boot.clone(),
                iface: iface.into(),
                temporary: Some(temporary.clone()),
                token: Some(token.clone()),
                uid: Some(uid),
            },
        )?;
        crate::checkpoint("link-saved");
        // A random staging name covers the gap between TUN creation and setting its alias.
        // The public floppaN name is exposed only after its ownership marker exists.
        self.cmd.ip(&[
            "tuntap",
            "add",
            "dev",
            &temporary,
            "mode",
            "tun",
            "user",
            &uid.to_string(),
        ])?;
        self.cmd.ip(&[
            "link",
            "set",
            "dev",
            &temporary,
            "alias",
            &format!("floppa-v2:{token}"),
        ])?;
        self.cmd
            .ip(&["link", "set", "dev", &temporary, "name", iface])?;
        Ok(())
    }
    pub fn recover_link(&self) -> Result<()> {
        let Some(Record::Link {
            boot,
            iface,
            temporary,
            token,
            uid,
        }) = self.journal.load("link")?
        else {
            return Ok(());
        };
        if boot != self.boot {
            return self.journal.clear("link");
        }
        let mut present = false;
        for name in std::iter::once(&iface).chain(temporary.iter()) {
            present |= self.link(name)?.is_some();
        }
        if !present {
            // There is nothing left to delete. Broken DNS/routing recovery should not
            // preserve a stale link record that no longer has any kernel object behind it.
            return self.journal.clear("link");
        }
        // Do not remove a link still needed for retrying per-link DNS or route recovery.
        ensure!(
            self.journal.load("dns")?.is_none()
                && self.journal.load("routes")?.is_none()
                && self.journal.load("policy")?.is_none(),
            "link recovery waits for DNS, policy and routes"
        );
        for name in std::iter::once(&iface).chain(temporary.iter()) {
            if let Some(link) = self.link(name)? {
                let owner = std::fs::read_to_string(format!("/sys/class/net/{name}/owner"))
                    .context("recorded interface is no longer a TUN")?;
                let flags = std::fs::read_to_string(format!("/sys/class/net/{name}/tun_flags"))?;
                let flags = u32::from_str_radix(flags.trim().trim_start_matches("0x"), 16)?;
                ensure!(
                    flags & libc::IFF_TUN as u32 != 0 && flags & libc::IFF_TAP as u32 == 0,
                    "recorded interface is not a TUN"
                );
                if let Some(t) = &token {
                    ensure!(
                        owner.trim().parse::<u32>().ok() == uid,
                        "tunnel owner changed"
                    );
                    let alias = link["ifalias"].as_str().unwrap_or("");
                    ensure!(
                        alias == format!("floppa-v2:{t}")
                            || (Some(name) == temporary.as_ref() && alias.is_empty()),
                        "tunnel identity changed"
                    );
                } else {
                    // Legacy journals carry no index/alias. Preserve any newly marked link.
                    ensure!(
                        link["ifalias"].as_str().unwrap_or("").is_empty(),
                        "legacy tunnel was replaced"
                    );
                }
                self.cmd
                    .ip(&["tuntap", "del", "dev", name, "mode", "tun"])?;
                ensure!(self.link(name)?.is_none(), "tunnel remains after removal");
            }
        }
        self.journal.clear("link")
    }
    pub fn add_routes(&self, iface: &str, destinations: &[String]) -> Result<()> {
        for f in ["-4", "-6"] {
            ensure!(
                !self.routes(f)?.iter().any(reserved_route),
                "reserved routing table already in use"
            );
        }
        let index = self.index(iface)?;
        self.journal.save(
            "routes",
            Record::Routes {
                boot: self.boot.clone(),
                iface: iface.into(),
                index: Some(index),
                destinations: Some(destinations.to_vec()),
            },
        )?;
        crate::checkpoint("routes-saved");
        for dest in destinations {
            let f = if crate::validate::network(dest)?.is_ipv4() {
                "-4"
            } else {
                "-6"
            };
            self.cmd.ip(&[
                f, "route", "add", dest, "dev", iface, "table", TABLE, "proto", "186",
            ])?;
        }
        Ok(())
    }
    pub fn recover_routes(&self) -> Result<()> {
        let Some(Record::Routes {
            boot,
            iface,
            index,
            destinations,
        }) = self.journal.load("routes")?
        else {
            return Ok(());
        };
        if boot != self.boot {
            return self.journal.clear("routes");
        }
        let mut errors = Vec::new();
        for f in ["-4", "-6"] {
            if let Err(e) = self.recover_route_family(f, &iface, index, destinations.as_deref()) {
                errors.push(format!("{f} routes: {e:#}"));
            }
        }
        ensure!(
            errors.is_empty(),
            "incomplete route recovery: {}",
            errors.join("; ")
        );
        self.journal.clear("routes")
    }
    fn recover_route_family(
        &self,
        f: &str,
        iface: &str,
        index: Option<u32>,
        destinations: Option<&[String]>,
    ) -> Result<()> {
        let routes = self.routes(f)?;
        let routes: Vec<_> = routes.iter().filter(|r| reserved_route(r)).collect();
        // An empty table needs no live link (it may already have disappeared).
        if !routes.is_empty() {
            if let Some(index) = index {
                ensure!(self.index(iface)? == index, "route interface was replaced");
            }
            for r in &routes {
                ensure!(r["dev"] == iface, "foreign device in reserved route table");
                let dest = r["dst"].as_str().context("missing route destination")?;
                let dest = if dest == "default" {
                    if f == "-4" { "0.0.0.0/0" } else { "::/0" }
                } else {
                    dest
                };
                if let Some(planned) = &destinations {
                    ensure!(
                        planned.iter().any(|p| canonical(p) == canonical(dest)),
                        "unrecorded route in reserved table"
                    );
                    ensure!(
                        r["protocol"] == "186" || r["protocol"] == 186,
                        "route protocol changed"
                    );
                } else {
                    ensure!(
                        r.get("protocol").is_none()
                            || r["protocol"] == "3"
                            || r["protocol"] == 3
                            || r["protocol"] == "boot",
                        "foreign legacy route protocol"
                    );
                }
                ensure!(
                    r.as_object().context("route object")?.keys().all(|k| [
                        "dst", "dev", "table", "protocol", "scope", "flags", "pref", "metric"
                    ]
                    .contains(&k.as_str())),
                    "foreign route attributes"
                );
                ensure!(
                    r.get("metric").is_none() || (f == "-6" && r["metric"] == 1024),
                    "route metric changed"
                );
                ensure!(
                    r["flags"]
                        .as_array()
                        .is_some_and(|f| f.iter().all(|v| v == "linkdown")),
                    "foreign route flags"
                );
            }
            for r in routes {
                let dest = r["dst"].as_str().unwrap();
                let mut args = vec![f, "route", "del", dest, "dev", iface, "table", TABLE];
                if destinations.is_some() {
                    args.extend(["proto", "186"]);
                }
                self.cmd.ip(&args)?;
            }
            ensure!(
                !self.routes(f)?.iter().any(reserved_route),
                "reserved routes remain"
            );
        }
        Ok(())
    }
    pub fn flush_address(&self, iface: &str, address: &str) -> Result<()> {
        if self.link(iface)?.is_none() {
            return Ok(());
        }
        let (ip, prefix) = address.split_once('/').context("address prefix")?;
        let ip: std::net::IpAddr = ip.parse()?;
        let prefix: u64 = prefix.parse()?;
        let info = self.cmd.json("ip", &["-j", "addr", "show", "dev", iface])?;
        let entries = info.as_array().context("invalid address array")?;
        for link in entries {
            for addr in link["addr_info"]
                .as_array()
                .context("invalid address info")?
            {
                if addr["local"]
                    .as_str()
                    .and_then(|s| s.parse::<std::net::IpAddr>().ok())
                    == Some(ip)
                    && addr["prefixlen"] == prefix
                {
                    self.cmd.ip(&["addr", "del", address, "dev", iface])?;
                }
            }
        }
        Ok(())
    }
}
fn canonical(s: &str) -> Option<String> {
    let (ip, prefix) = s
        .split_once('/')
        .unwrap_or((s, if s.contains(':') { "128" } else { "32" }));
    let ip: std::net::IpAddr = ip.parse().ok()?;
    let prefix: u8 = prefix.parse().ok()?;
    let ip = match ip {
        std::net::IpAddr::V4(ip) => std::net::IpAddr::V4(
            (u32::from(ip) & u32::MAX.checked_shl(u32::from(32 - prefix)).unwrap_or(0)).into(),
        ),
        std::net::IpAddr::V6(ip) => std::net::IpAddr::V6(
            (u128::from(ip) & u128::MAX.checked_shl(u32::from(128 - prefix)).unwrap_or(0)).into(),
        ),
    };
    Some(format!("{ip}/{prefix}"))
}
