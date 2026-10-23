use anyhow::{Context, Result, ensure};
use std::net::IpAddr;
pub fn iface(s: &str) -> Result<()> {
    ensure!(
        s.len() <= 15
            && s.strip_prefix("floppa")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())),
        "invalid interface name: {s}"
    );
    Ok(())
}
pub fn boot(s: &str) -> Result<()> {
    ensure!(
        s.len() == 36
            && s.bytes()
                .enumerate()
                .all(|(i, b)| if [8, 13, 18, 23].contains(&i) {
                    b == b'-'
                } else {
                    b.is_ascii_hexdigit()
                }),
        "invalid identity"
    );
    Ok(())
}
pub fn network(s: &str) -> Result<IpAddr> {
    let (addr, prefix) = s.split_once('/').context("network requires a prefix")?;
    let addr: IpAddr = addr.parse().context("invalid IP address")?;
    let prefix: u8 = prefix.parse().context("invalid prefix")?;
    ensure!(
        prefix <= if addr.is_ipv4() { 32 } else { 128 },
        "invalid prefix"
    );
    Ok(addr)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_shell_and_malformed_inputs() {
        for s in [
            "eth0",
            "floppa",
            "floppa0;id",
            "floppa12345678901",
            "../floppa0",
        ] {
            assert!(iface(s).is_err());
        }
        for s in [
            "999.1.1.1/8",
            "1.1.1.1/33",
            "::1/129",
            "1.1.1.1",
            "1.1.1.1/8\n",
        ] {
            assert!(network(s).is_err());
        }
        assert!(network("::/0").is_ok());
    }
}
