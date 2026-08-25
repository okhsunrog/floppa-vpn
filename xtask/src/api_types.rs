//! Generate the Rust API types from the OpenAPI document the server emits.
//!
//! The same document that feeds the TypeScript client (`floppa-web-shared/openapi.json`, written
//! by `floppa-server --openapi`) feeds this. Before it existed there were three hand-written
//! copies of the server's response bodies — one in the TS client, one in `floppa-cli`, one in the
//! Tauri client — each with its own idea of which fields exist and its own `Protocol` enum. Two of
//! those are now the same generated file, and the third is generated from the same source.
//!
//! # Why the document has to be reshaped first
//!
//! `typify` reads JSON Schema; OpenAPI 3.1 *contains* JSON Schema but keeps it under
//! `components/schemas` and points at it with `#/components/schemas/X`. So the schemas are lifted
//! into a `definitions` map and every `$ref` is repointed at it. Nothing else is touched: 3.1
//! schemas are draft 2020-12, which is what `typify` already accepts.
//!
//! Every schema in the document is generated, not a chosen subset. They are `pub` in a library,
//! so an unused one costs a few lines and no warning — and the alternative is a hand-kept list of
//! roots, which is the very thing this exists to abolish.

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use typify::{TypeSpace, TypeSpaceSettings};

/// Written by `floppa-server --openapi`, and committed.
const SPEC: &str = "floppa-web-shared/openapi.json";

/// Generated. The header says so, and `just openapi` rewrites it.
const OUT: &str = "floppa-api-client/src/schema.rs";

const HEADER: &str = "\
//! The server's API types, generated from `floppa-web-shared/openapi.json`.
//!
//! DO NOT EDIT. Regenerate with `just openapi`, which rebuilds the OpenAPI document from the
//! server's own annotations and then rewrites this file from it. Editing it by hand reintroduces
//! exactly the drift it exists to prevent.
#![allow(clippy::all)]

";

pub fn generate() -> Result<()> {
    let root = repo_root()?;
    let spec_path = root.join(SPEC);
    let spec: Value = serde_json::from_str(
        &std::fs::read_to_string(&spec_path)
            .with_context(|| format!("reading {}", spec_path.display()))?,
    )
    .with_context(|| format!("parsing {}", spec_path.display()))?;

    let schema = as_json_schema(&spec)?;
    let mut space = TypeSpace::new(TypeSpaceSettings::default().with_derive("PartialEq".into()));
    space
        .add_root_schema(serde_json::from_value(schema)?)
        .context("typify refused the reshaped document")?;

    let generated = prettyplease::unparse(
        &syn::parse2::<syn::File>(space.to_stream())
            .context("the generated tokens do not parse")?,
    );

    let out_path = root.join(OUT);
    if let Some(dir) = out_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&out_path, format!("{HEADER}{generated}"))
        .with_context(|| format!("writing {}", out_path.display()))?;
    println!("wrote {}", out_path.display());
    Ok(())
}

/// Lift `components/schemas` into a JSON Schema document `typify` can read.
fn as_json_schema(spec: &Value) -> Result<Value> {
    let Some(schemas) = spec
        .pointer("/components/schemas")
        .and_then(Value::as_object)
    else {
        bail!("the document has no components/schemas");
    };

    let mut definitions = Map::new();
    for (name, schema) in schemas {
        let mut schema = schema.clone();
        repoint_refs(&mut schema);
        definitions.insert(name.clone(), schema);
    }

    Ok(Value::Object(Map::from_iter([
        (
            "$schema".into(),
            Value::String("http://json-schema.org/draft-07/schema#".into()),
        ),
        // Named, because typify uses the title for the root type and a document with none gets
        // one invented from the file name.
        ("title".into(), Value::String("FloppaApi".into())),
        ("definitions".into(), Value::Object(definitions)),
    ])))
}

/// Rewrite every `#/components/schemas/X` into `#/definitions/X`, in place, at any depth.
fn repoint_refs(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.get_mut("$ref")
                && let Some(name) = reference.strip_prefix("#/components/schemas/")
            {
                *reference = format!("#/definitions/{name}");
            }
            for child in map.values_mut() {
                repoint_refs(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(repoint_refs),
        _ => {}
    }
}

/// The repository root, from this crate's manifest rather than the working directory.
fn repo_root() -> Result<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(Path::to_path_buf)
        .context("the xtask manifest has no parent directory")
}
