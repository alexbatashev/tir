//! LIT-style FileCheck tests for the TMDL compiler.
//!
//! Every file under `tmdl/checks` that contains a `RUN:` line is executed as an
//! individual `cargo test` case. The `RUN:` pipelines invoke the `tmdlc`
//! binary built by Cargo and verify its output with the in-process `filecheck`
//! matcher. Regenerate the `CHECK` lines with
//! `./utils/scripts/update_checks.py tmdl`.

fn main() {
    tir_lit::harness_main(
        env!("CARGO_MANIFEST_DIR"),
        "checks",
        &[("tmdlc", env!("CARGO_BIN_EXE_tmdlc"))],
    );
}
