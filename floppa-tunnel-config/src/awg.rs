//! AmneziaWG 2.0 obfuscation parameters.
//!
//! One type serves both ends: the server reads it from `config.toml` (defaults filled in by
//! serde) and renders it into client configs and `awg set` arguments; the clients parse it back
//! out of the `[Interface]` section of an AmneziaWG `.conf` and hand it to gotatun.

use serde::{Deserialize, Serialize};

/// AmneziaWG 2.0 obfuscation parameters. [`Default`] is the recommended (Amnezia) preset.
///
/// `H1`–`H4` and `S1`–`S4` are bidirectional and must match on both ends. `Jc`/`Jmin`/`Jmax`
/// (junk packets) and `I1`–`I5` (signature packets) are initiator-only (sent by the client),
/// but the full set is stored centrally and applied to both the server interface and clients.
///
/// An empty `I` slot means "unset". Older client builds stored those slots as `null`; that still
/// reads back as empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwgObfuscation {
    /// Junk packet count sent before the handshake (initiator only).
    #[serde(default = "awg_default_jc")]
    pub jc: u32,
    /// Minimum junk packet size in bytes.
    #[serde(default = "awg_default_jmin")]
    pub jmin: u32,
    /// Maximum junk packet size in bytes (keep below MTU).
    #[serde(default = "awg_default_jmax")]
    pub jmax: u32,
    /// Padding prepended to Handshake Initiation.
    #[serde(default = "awg_default_s1")]
    pub s1: u32,
    /// Padding prepended to Handshake Response.
    #[serde(default = "awg_default_s2")]
    pub s2: u32,
    /// Padding prepended to Cookie Reply (AmneziaWG 2.0).
    #[serde(default = "awg_default_s3")]
    pub s3: u32,
    /// Padding prepended to Transport Data (AmneziaWG 2.0).
    #[serde(default = "awg_default_s4")]
    pub s4: u32,
    /// Magic header for Handshake Initiation. A single value ("1") or range ("234567-345678").
    #[serde(default = "awg_default_h1")]
    pub h1: String,
    /// Magic header for Handshake Response.
    #[serde(default = "awg_default_h2")]
    pub h2: String,
    /// Magic header for Cookie Reply.
    #[serde(default = "awg_default_h3")]
    pub h3: String,
    /// Magic header for Transport Data.
    #[serde(default = "awg_default_h4")]
    pub h4: String,
    /// Signature packet 1 (AmneziaWG 2.0 CPS) — protocol-mimicry tag spec. Empty = unset.
    #[serde(default = "awg_default_i1", with = "i_slot")]
    pub i1: String,
    /// Signature packet 2. Empty = unset.
    #[serde(default, with = "i_slot")]
    pub i2: String,
    /// Signature packet 3. Empty = unset.
    #[serde(default, with = "i_slot")]
    pub i3: String,
    /// Signature packet 4. Empty = unset.
    #[serde(default, with = "i_slot")]
    pub i4: String,
    /// Signature packet 5. Empty = unset.
    #[serde(default, with = "i_slot")]
    pub i5: String,
}

/// Serde form of an `I` slot: `Option<String>`, `None` for "unset", in BOTH directions.
///
/// Symmetry is the point. An earlier version deserialized an `Option` (so that the `null` older
/// client builds stored still read back as empty) but serialized a plain `String`. JSON never
/// noticed; bincode — the format of the UI ↔ `:vpn` RPC — did: it is not self-describing, so the
/// reader took the string's length byte for an `Option` tag and every AmneziaWG `start_tunnel`
/// failed to decode ("tag for enum is not valid"). With the same shape on both sides every
/// format round-trips, and the stored JSON is exactly what older builds wrote (`null`).
mod i_slot {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(value: &str, serializer: S) -> Result<S::Ok, S::Error> {
        if value.is_empty() {
            None::<&str>.serialize(serializer)
        } else {
            Some(value).serialize(serializer)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
        Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
    }
}

// Recommended AmneziaWG 2.0 preset (Amnezia default preset).
fn awg_default_jc() -> u32 {
    6
}
fn awg_default_jmin() -> u32 {
    55
}
fn awg_default_jmax() -> u32 {
    205
}
fn awg_default_s1() -> u32 {
    72
}
fn awg_default_s2() -> u32 {
    56
}
fn awg_default_s3() -> u32 {
    32
}
fn awg_default_s4() -> u32 {
    16
}
fn awg_default_h1() -> String {
    "234567-345678".to_string()
}
fn awg_default_h2() -> String {
    "3456789-4567890".to_string()
}
fn awg_default_h3() -> String {
    "56789012-67890123".to_string()
}
fn awg_default_h4() -> String {
    "456789012-567890123".to_string()
}
/// QUIC v1 long-header mimic for the first signature packet.
fn awg_default_i1() -> String {
    "<b 0xc30000000108><r 8><b 0x08><r 8><b 0x0045dc><t><r 16>".to_string()
}

impl Default for AwgObfuscation {
    fn default() -> Self {
        Self {
            jc: awg_default_jc(),
            jmin: awg_default_jmin(),
            jmax: awg_default_jmax(),
            s1: awg_default_s1(),
            s2: awg_default_s2(),
            s3: awg_default_s3(),
            s4: awg_default_s4(),
            h1: awg_default_h1(),
            h2: awg_default_h2(),
            h3: awg_default_h3(),
            h4: awg_default_h4(),
            i1: awg_default_i1(),
            i2: String::new(),
            i3: String::new(),
            i4: String::new(),
            i5: String::new(),
        }
    }
}

impl AwgObfuscation {
    /// Plain WireGuard behaviour: no junk, no padding, the standard message types 1–4 as headers,
    /// no signature packets. The baseline a parsed `.conf` starts from, so a key the file leaves
    /// out keeps the WireGuard meaning rather than silently picking up the preset.
    pub fn wireguard() -> Self {
        Self {
            jc: 0,
            jmin: 0,
            jmax: 0,
            s1: 0,
            s2: 0,
            s3: 0,
            s4: 0,
            h1: "1".into(),
            h2: "2".into(),
            h3: "3".into(),
            h4: "4".into(),
            i1: String::new(),
            i2: String::new(),
            i3: String::new(),
            i4: String::new(),
            i5: String::new(),
        }
    }

    /// The signature packet specs `I1`–`I5` in order, `None` where the slot is unset.
    pub fn signature_packets(&self) -> [Option<&str>; 5] {
        [&self.i1, &self.i2, &self.i3, &self.i4, &self.i5]
            .map(|spec| (!spec.is_empty()).then_some(spec.as_str()))
    }
}

#[cfg(test)]
mod tests {

    /// `null` (what older client builds stored), `""` and an absent key all mean "unset", and
    /// an unset slot is written back as `null` — the same shape in both directions, which is
    /// what keeps the type decodable from a non-self-describing format like bincode.
    #[test]
    fn i_slots_read_null_empty_and_absent_as_unset_and_write_null() {
        let json = r#"{"jc":6,"jmin":55,"jmax":205,"s1":72,"s2":56,"s3":32,"s4":16,
            "h1":"1","h2":"2","h3":"3","h4":"4","i1":null,"i2":"","i3":"<b 0x01>"}"#;
        let parsed: AwgObfuscation = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.i1, "");
        assert_eq!(parsed.i2, "");
        assert_eq!(parsed.i3, "<b 0x01>");
        assert_eq!(parsed.i4, "");
        let written: serde_json::Value = serde_json::to_value(&parsed).unwrap();
        assert_eq!(written["i1"], serde_json::Value::Null);
        assert_eq!(written["i3"], "<b 0x01>");
        let again: AwgObfuscation = serde_json::from_value(written).unwrap();
        assert_eq!(again, parsed);
    }
    use super::*;

    #[test]
    fn the_default_is_the_recommended_preset_and_toml_fills_in_what_is_missing() {
        let preset = AwgObfuscation::default();
        assert_eq!(preset.jc, 6);
        assert_eq!(preset.h1, "234567-345678");
        assert!(preset.i1.starts_with("<b 0xc30000000108>"));
        assert_eq!(preset.signature_packets()[1], None);

        let partial: AwgObfuscation = toml::from_str("jc = 3\ni1 = \"\"").unwrap();
        assert_eq!(partial.jc, 3);
        assert_eq!(partial.i1, "");
        assert_eq!(partial.h4, preset.h4);
    }

    #[test]
    fn null_signature_slots_from_an_older_store_read_as_unset() {
        // The clients used to persist `I1`–`I5` as `Option<String>`.
        let json = r#"{"jc":4,"jmin":40,"jmax":70,"s1":15,"s2":18,"s3":0,"s4":0,
            "h1":"5-10","h2":"2","h3":"3","h4":"4",
            "i1":"<b 0xf6>","i2":null,"i3":null,"i4":null,"i5":null}"#;
        let o: AwgObfuscation = serde_json::from_str(json).unwrap();
        assert_eq!(o.i1, "<b 0xf6>");
        assert_eq!(o.i2, "");
        assert_eq!(
            o.signature_packets(),
            [Some("<b 0xf6>"), None, None, None, None]
        );

        let again: AwgObfuscation =
            serde_json::from_str(&serde_json::to_string(&o).unwrap()).unwrap();
        assert_eq!(again, o);
    }

    #[test]
    fn the_wireguard_baseline_is_no_obfuscation() {
        let wg = AwgObfuscation::wireguard();
        assert_eq!((wg.jc, wg.jmin, wg.jmax), (0, 0, 0));
        assert_eq!((wg.s1, wg.s2, wg.s3, wg.s4), (0, 0, 0, 0));
        assert_eq!([&wg.h1, &wg.h2, &wg.h3, &wg.h4], ["1", "2", "3", "4"]);
        assert_eq!(wg.signature_packets(), [None; 5]);
    }
}
