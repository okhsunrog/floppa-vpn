//! Repository chores that need Rust rather than a shell line.
//!
//! Run through `just`, never directly — the recipes are where the paths live.

use anyhow::{Context, Result, bail};

mod api_types;

fn main() -> Result<()> {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("api-types") => api_types::generate().context("generating the API types"),
        Some(other) => bail!("unknown task `{other}`; known tasks: api-types"),
        None => bail!("usage: cargo run -p xtask -- <task>"),
    }
}
