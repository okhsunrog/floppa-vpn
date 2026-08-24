//! Write `floppa-client/src/bindings.ts` and exit.
//!
//! `bindings.ts` is generated from the Rust command and event definitions, and the only way to
//! regenerate it used to be starting the whole app — a window, a webview, a dev server on :1420 and
//! a tunnel actor, all so a type reflection pass could run and write one file. This binary runs
//! that pass on its own.
//!
//! The path is resolved from the manifest directory rather than the working directory, so it does
//! not matter where this is invoked from.

fn main() {
    let out = concat!(env!("CARGO_MANIFEST_DIR"), "/../src/bindings.ts");

    match floppa_client_lib::export_bindings(out) {
        Ok(()) => println!("wrote {out}"),
        Err(e) => {
            eprintln!("failed to export bindings: {e}");
            std::process::exit(1);
        }
    }
}
