//! Teaching clap the one [`Protocol`] there is.
//!
//! There used to be a second enum here, with the same three variants, its own `Display` and its
//! own conversion to the server's `PeerProtocol` — because clap wants a type it can derive
//! `ValueEnum` for, and the core's protocol is not one. Two enums over the same three things drift,
//! and the way this pair would have drifted is a name: the derive spells variants in kebab-case, so
//! `AmneziaWg` becomes `amnezia-wg`, and keeping it as `amneziawg` means a `#[value(name = …)]`
//! beside every `#[serde(rename = …)]` — two spellings of one string, in two crates, with nothing
//! to notice when only one of them is changed.
//!
//! So clap is told the values instead of deriving them, and told them out of [`Protocol::ALL`] and
//! [`Protocol::as_str`]. A protocol added to the core appears here with no edit, under the name the
//! core gives it, and `--help` lists exactly what `FromStr` accepts.

use clap::builder::{PossibleValue, PossibleValuesParser, TypedValueParser};
use floppa_vpn_core::protocol::Protocol;

/// Accepts exactly the protocol names the core knows, in its own preference order — so the default
/// is listed first in `--help`, which is also what it is.
pub fn parser() -> impl TypedValueParser<Value = Protocol> {
    PossibleValuesParser::new(Protocol::ALL.map(|p| PossibleValue::new(p.as_str()))).map(|name| {
        name.parse::<Protocol>()
            .expect("clap only ever yields a name that came from Protocol::ALL")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reason the parser is built from the type rather than written out: every name clap
    /// offers has to be one `FromStr` accepts, and a protocol added to the core has to arrive here
    /// without anyone remembering to come back.
    #[test]
    fn every_name_clap_offers_is_one_the_core_parses_back() {
        for protocol in Protocol::ALL {
            assert_eq!(
                protocol.as_str().parse::<Protocol>(),
                Ok(protocol),
                "clap would offer `{}` and the core would refuse it",
                protocol.as_str()
            );
        }
    }
}
