//! Rules for types that cross the specta/TypeScript boundary.
//!
//! # Internally tagged enums are safe here — but only because of the serde format layer
//!
//! `specta-typescript` on its own knows nothing about `#[serde(tag = "...")]`: the derive macro
//! records the attribute as opaque metadata (`serde:container:tag`) and the TypeScript exporter
//! never reads that key. What makes internal tagging come out right is
//! [`specta_serde`], a [`specta::Format`] implementation that rewrites the collected datatypes
//! into their serde representation (`EnumRepr::Internal { tag }` →
//! `transform_internal_variant`) *before* the exporter runs. `tauri-specta` applies it on every
//! export (`lang/js_ts.rs`: `specta_serde::PhasesFormat.map_type(...)`).
//!
//! The consequence worth remembering: a discriminated union is only correctly declared while that
//! layer stays in the pipeline. If an upgrade ever drops or bypasses it, serde would keep emitting
//! `{"kind":"verify_failed", ...}` while the generated `.d.ts` reverted to the externally tagged
//! `{"VerifyFailed":{...}}` — nothing would fail loudly, the TypeScript `switch` would simply stop
//! matching. That is exactly the silent presentation-layer failure this refactor exists to remove,
//! so the test below pins the behaviour rather than trusting it.
//!
//! Both shapes below are therefore permitted for types crossing the boundary:
//!
//! - `#[serde(tag = "kind")]` on an enum with struct variants — a true sum type;
//! - a struct with an explicit enum discriminant field, the shape `ConnectError { code, message }`
//!   already uses — preferable when every variant carries the same payload.

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use specta::{Type, Types};
    use specta_typescript::Typescript;

    #[derive(Serialize, Deserialize, Type)]
    #[serde(rename_all = "snake_case")]
    enum ProbeKind {
        VerifyFailed,
        PermissionDenied,
    }

    /// Discriminant-field shape.
    #[derive(Serialize, Deserialize, Type)]
    struct ProbeFlat {
        kind: ProbeKind,
        detail: String,
    }

    /// Internally tagged sum type — the shape `AttemptError` / `CycleOutcome` use.
    #[derive(Serialize, Deserialize, Type)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum ProbeInternallyTagged {
        VerifyFailed { detail: String },
        PermissionDenied { code: i32 },
    }

    /// Export exactly the way `tauri-specta` does: through the serde format layer.
    fn export<T: Type>() -> String {
        let types = Types::default().register::<T>();
        Typescript::default()
            .export(&types, specta_serde::Format)
            .expect("export")
    }

    #[test]
    fn discriminant_field_shape_matches_its_json() {
        let json = serde_json::to_string(&ProbeFlat {
            kind: ProbeKind::VerifyFailed,
            detail: "x".into(),
        })
        .unwrap();
        assert_eq!(json, r#"{"kind":"verify_failed","detail":"x"}"#);

        let ts = export::<ProbeFlat>();
        assert!(ts.contains(r#""verify_failed""#), "no discriminant: {ts}");
    }

    #[test]
    fn internally_tagged_enum_is_declared_the_way_serde_serializes_it() {
        let json =
            serde_json::to_string(&ProbeInternallyTagged::VerifyFailed { detail: "x".into() })
                .unwrap();
        assert_eq!(json, r#"{"kind":"verify_failed","detail":"x"}"#);

        let ts = export::<ProbeInternallyTagged>();

        // The declaration must carry the tag inline, as a narrowable literal — not the
        // externally tagged `{ VerifyFailed: { ... } }` wrapper.
        assert!(
            ts.contains(r#"kind: "verify_failed""#),
            "serde format layer is not rewriting internal tags; a `switch (e.kind)` in \
             TypeScript would silently stop matching. Emitted:\n{ts}"
        );
        assert!(
            !ts.contains("VerifyFailed:"),
            "externally tagged wrapper leaked into the declaration:\n{ts}"
        );
    }
}
