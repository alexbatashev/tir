//! LIT-style FileCheck tests for `tir-mc`, the codegen driver.
//!
//! Every file under `utils/tir-mc/checks` that contains a `RUN:` line runs as an
//! individual `cargo test` case. The tests feed textual TIR through `tir-mc`
//! (selecting a target with `--march` and running `isel`/`regalloc`) and verify
//! the lowered machine IR with the in-process `filecheck` matcher. They are
//! organised by target (`riscv`, `arm64`) and pass (`isel`, `regalloc`).

fn main() {
    tir_lit::harness_main(
        env!("CARGO_MANIFEST_DIR"),
        "checks",
        &[("tir-mc", env!("CARGO_BIN_EXE_tir-mc"))],
    );
}
