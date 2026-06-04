//! LIT-style FileCheck tests for `isasim`, the ISA simulator.
//!
//! Every file under `simulator/isasim/checks` that contains a `RUN:` line runs
//! as an individual `cargo test` case. The tests assemble a `.S` program for a
//! target (`--march`), simulate it, and verify the instruction trace (parser
//! coverage) or the final register state (execution correctness) with the
//! in-process `filecheck` matcher. They are organised by target (`riscv`,
//! `arm64`) and kind (`parse`, `exec`).

fn main() {
    tir_lit::harness_main(
        env!("CARGO_MANIFEST_DIR"),
        "checks",
        &[("isasim", env!("CARGO_BIN_EXE_tir-isasim"))],
    );
}
