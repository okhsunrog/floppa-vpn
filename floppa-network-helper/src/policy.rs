use crate::{Context as Helper, journal::Record};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
pub const MARK: u64 = 0x666c6f70;
pub const TABLE: &str = "1718382448";
pub const MARK_HEX: &str = "0x666c6f70";

fn expr(pre: bool) -> Value {
    let ct = json!({"ct":{"key":"mark"}});
    let meta = json!({"meta":{"key":"mark"}});
    let (source, dest) = if pre { (ct, meta) } else { (meta, ct) };
    json!([{"match":{"op":"==", "left":source, "right":MARK}}, {"mangle":{"key":dest,"value":source}}])
}
/// Full structural match, not a name/prefix check. Ignore only nft object handles.
/// Legacy duplicates are accepted, but extra expressions, chains, sets and comments are not.
pub fn table_identity(value: &Value) -> Result<Option<String>> {
    let entries = value["nftables"]
        .as_array()
        .context("missing nftables array")?;
    let mut table = false;
    let mut comment = None;
    let mut chains = [false; 2];
    let mut rules = [0; 2];
    for entry in entries {
        let obj = entry.as_object().context("invalid nft object")?;
        ensure!(obj.len() == 1, "unexpected nft object");
        let (kind, value) = obj.iter().next().unwrap();
        if kind == "metainfo" {
            continue;
        }
        let mut value = value.clone();
        value
            .as_object_mut()
            .context("invalid nft object body")?
            .remove("handle");
        match kind.as_str() {
            "table" => {
                ensure!(!table, "duplicate table");
                table = true;
                comment = value
                    .as_object_mut()
                    .unwrap()
                    .remove("comment")
                    .map(|v| {
                        v.as_str()
                            .map(str::to_string)
                            .context("invalid table comment")
                    })
                    .transpose()?;
                ensure!(
                    value == json!({"family":"inet","name":"floppa_policy"}),
                    "foreign table attributes"
                );
            }
            "chain" => {
                let name = value["name"]
                    .as_str()
                    .context("missing chain name")?
                    .to_string();
                let index = chain_index(&name)?;
                ensure!(!chains[index], "duplicate chain");
                chains[index] = true;
                ensure!(
                    value
                        == json!({"family":"inet","table":"floppa_policy","name":name,"type":"filter","hook":name,"prio":-150,"policy":"accept"}),
                    "foreign chain attributes"
                );
            }
            "rule" => {
                let name = value["chain"]
                    .as_str()
                    .context("missing rule chain")?
                    .to_string();
                let index = chain_index(&name)?;
                ensure!(
                    value
                        == json!({"family":"inet","table":"floppa_policy","chain":name,"expr":expr(index == 0)}),
                    "foreign rule in floppa_policy"
                );
                rules[index] += 1;
            }
            _ => bail!("foreign {kind} in floppa_policy"),
        }
    }
    ensure!(
        table && chains.iter().all(|v| *v) && rules.iter().all(|n| *n > 0),
        "incomplete Floppa table"
    );
    Ok(comment)
}
fn chain_index(name: &str) -> Result<usize> {
    match name {
        "prerouting" => Ok(0),
        "postrouting" => Ok(1),
        _ => bail!("foreign chain {name}"),
    }
}
pub fn reserved_rule(v: &Value) -> bool {
    matches!(v["priority"].as_u64(), Some(31000 | 31010))
}
fn numeric(v: &Value, n: u64) -> bool {
    v.as_u64() == Some(n) || v.as_str().is_some_and(|s| s.parse::<u64>() == Ok(n))
}
pub fn reserved_route(v: &Value) -> bool {
    numeric(&v["table"], MARK)
}
pub fn owned_rule(v: &Value, legacy: bool) -> bool {
    let Some(priority) = v["priority"].as_u64() else {
        return false;
    };
    if ![31000, 31010].contains(&priority) {
        return false;
    }
    let mut normalized = v.clone();
    if numeric(
        &normalized["table"],
        if priority == 31000 { 254 } else { MARK },
    ) {
        normalized["table"] = json!(if priority == 31000 { "main" } else { TABLE });
    }
    if numeric(&normalized["fwmark"], MARK) {
        normalized["fwmark"] = json!(MARK_HEX);
    }
    let mut expected = json!({"priority":priority,"not":null,"src":"all","fwmark":MARK_HEX,"table":if priority == 31000 {"main"} else {TABLE}});
    if priority == 31000 {
        expected["suppress_prefixlen"] = json!(0);
    }
    if !legacy {
        expected["protocol"] = json!("186");
        if normalized["protocol"] == 186 {
            normalized["protocol"] = json!("186");
        }
    }
    normalized == expected
}
impl Helper {
    pub fn tables(&self) -> Result<Option<Value>> {
        let list = self.cmd.json("nft", &["-j", "list", "tables"])?;
        let list = list["nftables"]
            .as_array()
            .context("missing nftables array")?;
        if !list
            .iter()
            .any(|v| v["table"]["name"] == "floppa_policy" && v["table"]["family"] == "inet")
        {
            return Ok(None);
        }
        self.cmd
            .json("nft", &["-j", "list", "table", "inet", "floppa_policy"])
            .map(Some)
    }
    pub fn rules(&self, family: &str) -> Result<Vec<Value>> {
        self.cmd
            .json("ip", &["-j", "-N", family, "rule", "show"])?
            .as_array()
            .cloned()
            .context("invalid ip rule array")
    }
    pub fn routes(&self, family: &str) -> Result<Vec<Value>> {
        self.cmd
            .json("ip", &["-j", "-N", family, "route", "show", "table", "all"])?
            .as_array()
            .cloned()
            .context("invalid ip route array")
    }
    pub fn check_policy(&self) -> Result<()> {
        ensure!(
            self.tables()?.is_none(),
            "reserved nftables table floppa_policy is already in use; recover the previous tunnel first"
        );
        for f in ["-4", "-6"] {
            ensure!(
                !self.rules(f)?.iter().any(reserved_rule)
                    && !self.routes(f)?.iter().any(reserved_route),
                "reserved {f} routing rules/table already in use"
            );
        }
        Ok(())
    }
    pub fn add_policy(&self, ipv4: bool, ipv6: bool) -> Result<()> {
        ensure!(
            self.tables()?.is_none(),
            "reserved nftables table already in use"
        );
        for f in ["-4", "-6"] {
            ensure!(
                !self.rules(f)?.iter().any(reserved_rule),
                "reserved rule priorities already in use"
            );
        }
        let old_mark = if ipv4 {
            let m = std::fs::read_to_string("/proc/sys/net/ipv4/conf/all/src_valid_mark")?
                .trim()
                .parse::<u8>()?;
            ensure!(m <= 1, "invalid src_valid_mark");
            Some(m)
        } else {
            None
        };
        let token = crate::new_id()?;
        self.journal.save(
            "policy",
            Record::Policy {
                boot: self.boot.clone(),
                ipv4,
                ipv6,
                old_mark,
                token: Some(token.clone()),
            },
        )?;
        crate::checkpoint("policy-saved");
        // Exclusive creation plus a single nft transaction: a racing foreign table never
        // receives our rules. The saved token also prevents undo from deleting that table.
        self.cmd.run("nft", &["-f","-"], Some(&format!("create table inet floppa_policy {{ comment \"floppa-v2:{token}\"; }}\nadd chain inet floppa_policy prerouting {{ type filter hook prerouting priority mangle; policy accept; }}\nadd rule inet floppa_policy prerouting ct mark {MARK_HEX} meta mark set ct mark\nadd chain inet floppa_policy postrouting {{ type filter hook postrouting priority mangle; policy accept; }}\nadd rule inet floppa_policy postrouting meta mark {MARK_HEX} ct mark set meta mark\n")))?;
        crate::checkpoint("policy-table");
        if ipv4 {
            std::fs::write("/proc/sys/net/ipv4/conf/all/src_valid_mark", "1\n")?;
        }
        crate::checkpoint("policy-mark");
        for (f, enabled) in [("-4", ipv4), ("-6", ipv6)] {
            if enabled {
                self.rule_command(f, "add", 31000, false)?;
                self.rule_command(f, "add", 31010, false)?;
            }
        }
        Ok(())
    }
    fn rule_command(&self, family: &str, action: &str, priority: u64, legacy: bool) -> Result<()> {
        let mut args = vec![
            family,
            "rule",
            action,
            "priority",
            if priority == 31000 { "31000" } else { "31010" },
            "not",
            "fwmark",
            MARK_HEX,
            "lookup",
            if priority == 31000 { "main" } else { TABLE },
        ];
        if priority == 31000 {
            args.extend(["suppress_prefixlength", "0"]);
        }
        if !legacy {
            args.extend(["protocol", "186"]);
        }
        self.cmd.ip(&args)
    }
    pub fn recover_policy(&self) -> Result<()> {
        let Some(Record::Policy {
            boot,
            ipv4,
            ipv6,
            old_mark,
            token,
        }) = self.journal.load("policy")?
        else {
            return self.recover_orphan_table();
        };
        if boot != self.boot {
            return self.journal.clear("policy");
        }
        // Validate every owned object before undo. A collision never authorizes deletion.
        let table = self.tables()?;
        if let Some(table) = &table {
            let comment = table_identity(table)?;
            ensure!(
                comment == token.as_ref().map(|t| format!("floppa-v2:{t}")),
                "nft table ownership changed; keeping recovery record"
            );
        }
        for (f, enabled) in [("-4", ipv4), ("-6", ipv6)] {
            if enabled {
                let rules = self.rules(f)?;
                ensure!(
                    rules
                        .iter()
                        .filter(|r| reserved_rule(r))
                        .all(|r| owned_rule(r, token.is_none())),
                    "foreign reserved rule; keeping recovery record"
                );
            }
        }
        for (f, enabled) in [("-4", ipv4), ("-6", ipv6)] {
            if enabled {
                for r in self.rules(f)?.iter().filter(|r| reserved_rule(r)) {
                    ensure!(
                        owned_rule(r, token.is_none()),
                        "routing rule changed during recovery"
                    );
                    self.rule_command(f, "del", r["priority"].as_u64().unwrap(), token.is_none())?;
                }
                ensure!(
                    !self.rules(f)?.iter().any(reserved_rule),
                    "reserved rules remain"
                );
            }
        }
        if let Some(table) = table {
            self.delete_table(&table)?;
        }
        if let Some(mark) = old_mark {
            let current = std::fs::read_to_string("/proc/sys/net/ipv4/conf/all/src_valid_mark")?;
            // A changed value belongs to the administrator; don't overwrite it.
            if current.trim() == "1" {
                std::fs::write(
                    "/proc/sys/net/ipv4/conf/all/src_valid_mark",
                    format!("{mark}\n"),
                )?;
            }
        }
        crate::checkpoint("policy-restored");
        self.journal.clear("policy")
    }
    fn delete_table(&self, table: &Value) -> Result<()> {
        // Delete by kernel handle, never by a name that may have been reused after inspection.
        let handle = table["nftables"]
            .as_array()
            .context("nftables")?
            .iter()
            .find_map(|v| v["table"]["handle"].as_u64())
            .context("missing table handle")?
            .to_string();
        self.cmd
            .run("nft", &["delete", "table", "inet", "handle", &handle], None)?;
        crate::checkpoint("policy-deleted");
        ensure!(
            self.tables()?.is_none(),
            "reserved table remains after delete"
        );
        Ok(())
    }
    fn recover_orphan_table(&self) -> Result<()> {
        let Some(table) = self.tables()? else {
            return Ok(());
        };
        // Narrow migration for the released Bash bug: unmarked canonical table (possibly
        // duplicate rules), no surviving routing state, no policy record. Never guess sysctl.
        ensure!(
            table_identity(&table)?.is_none(),
            "unrecorded marked table; refusing to guess ownership"
        );
        for f in ["-4", "-6"] {
            ensure!(
                !self.rules(f)?.iter().any(reserved_rule)
                    && !self.routes(f)?.iter().any(reserved_route),
                "orphan table has unrecorded routing state; manual recovery required"
            );
        }
        eprintln!("Recovering legacy Floppa nftables table left without a journal");
        self.delete_table(&table)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn foreign_rule_is_not_owned() {
        let mut r =
            json!({"priority":31010,"not":null,"src":"all","fwmark":MARK_HEX,"table":TABLE});
        assert!(owned_rule(&r, true));
        r["iif"] = json!("eth0");
        assert!(!owned_rule(&r, true));
    }
    #[test]
    fn rejects_extra_nft_objects() {
        assert!(
            table_identity(
                &json!({"nftables":[{"table":{"family":"inet","name":"floppa_policy"}},{"set":{}}]})
            )
            .is_err()
        );
    }
}
