//! The inline-keyboard callback protocol.
//!
//! Every button the bot sends carries a [`CallbackAction`] rendered with `Display`; every tap
//! comes back as `callback_data` parsed with `FromStr`. Keeping both sides in one type means a
//! button can never be built with data the handler does not understand, and vice versa.
//!
//! Telegram limits `callback_data` to 64 bytes; the test below checks every variant against
//! that with its largest realistic payload.

use floppa_core::models::Lang;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackAction {
    /// Switch the interface language (buttons under `/lang`).
    SetLang(Lang),
    /// Start buying a plan (buttons under `/buy` and in expiry notifications).
    Buy { plan_id: i32 },
    /// Confirm folding this Telegram's established account into the account that minted the
    /// link code (the `/start link_<code>` merge prompt).
    LinkMerge { code: String },
    /// Dismiss the merge prompt without changes.
    LinkCancel,
}

impl fmt::Display for CallbackAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallbackAction::SetLang(lang) => write!(f, "lang:{lang}"),
            CallbackAction::Buy { plan_id } => write!(f, "buy:{plan_id}"),
            CallbackAction::LinkMerge { code } => write!(f, "link_merge:{code}"),
            CallbackAction::LinkCancel => f.write_str("link_cancel"),
        }
    }
}

/// `callback_data` that no button of ours produces — a stale button from an older bot
/// version, or a client sending made-up data.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unrecognised callback data {0:?}")]
pub struct CallbackParseError(pub String);

impl FromStr for CallbackAction {
    type Err = CallbackParseError;

    fn from_str(data: &str) -> Result<Self, Self::Err> {
        let unknown = || CallbackParseError(data.to_owned());
        if let Some(lang) = data.strip_prefix("lang:") {
            return lang
                .parse()
                .map(CallbackAction::SetLang)
                .map_err(|_| unknown());
        }
        if let Some(plan_id) = data.strip_prefix("buy:") {
            return plan_id
                .parse()
                .map(|plan_id| CallbackAction::Buy { plan_id })
                .map_err(|_| unknown());
        }
        if let Some(code) = data.strip_prefix("link_merge:") {
            if code.is_empty() {
                return Err(unknown());
            }
            return Ok(CallbackAction::LinkMerge {
                code: code.to_owned(),
            });
        }
        if data == "link_cancel" {
            return Ok(CallbackAction::LinkCancel);
        }
        Err(unknown())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Telegram's `callback_data` limit, in bytes.
    const TELEGRAM_CALLBACK_DATA_MAX: usize = 64;

    /// Link codes are 32 hex digits (`admin::routes::auth::generate_link_code`).
    const LINK_CODE: &str = "0123456789abcdef0123456789abcdef";

    fn every_variant_at_its_largest() -> Vec<CallbackAction> {
        vec![
            CallbackAction::SetLang(Lang::En),
            CallbackAction::SetLang(Lang::Ru),
            CallbackAction::Buy { plan_id: i32::MAX },
            CallbackAction::Buy { plan_id: i32::MIN },
            CallbackAction::LinkMerge {
                code: LINK_CODE.to_owned(),
            },
            CallbackAction::LinkCancel,
        ]
    }

    #[test]
    fn every_variant_round_trips_within_telegram_limit() {
        for action in every_variant_at_its_largest() {
            let rendered = action.to_string();
            assert!(
                rendered.len() <= TELEGRAM_CALLBACK_DATA_MAX,
                "{rendered:?} is {} bytes",
                rendered.len()
            );
            assert_eq!(
                rendered.parse::<CallbackAction>(),
                Ok(action),
                "{rendered:?}"
            );
        }
    }

    #[test]
    fn wire_format_is_stable() {
        // Buttons already sitting in users' chats were rendered with these exact strings.
        assert_eq!(CallbackAction::SetLang(Lang::Ru).to_string(), "lang:ru");
        assert_eq!(CallbackAction::Buy { plan_id: 7 }.to_string(), "buy:7");
        assert_eq!(
            CallbackAction::LinkMerge { code: "abc".into() }.to_string(),
            "link_merge:abc"
        );
        assert_eq!(CallbackAction::LinkCancel.to_string(), "link_cancel");
    }

    #[test]
    fn garbage_is_rejected_not_guessed() {
        for data in [
            "",
            "lang:",
            "lang:de",
            "buy:",
            "buy:x",
            "link_merge:",
            "cancel",
            "buy",
        ] {
            assert!(data.parse::<CallbackAction>().is_err(), "{data:?}");
        }
    }
}
