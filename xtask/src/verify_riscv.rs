use std::path::PathBuf;
use xshell::{cmd, Shell};

use super::utils::{cmake_build, cmake_configure, emit_rocq_riscv, git_checkout, project_root};

pub fn verify_riscv(sh: &Shell) -> anyhow::Result<()> {
    // 1) Checkout sail-riscv under target/<dest_dir>
    git_checkout(
        sh,
        "https://github.com/riscv/sail-riscv.git",
        "0.8",
        "verify/_deps/sail-riscv",
    )?;

    // 2) Configure CMake for sail-riscv
    let root = project_root();
    let sail_src: PathBuf = root.join("target/verify/_deps/sail-riscv");
    let sail_build: PathBuf = sail_src.join("build");
    if std::env::var("TIR_SKIP_SAIL_FETCH").ok().as_deref() != Some("1") {
        cmake_configure(sh, &sail_src, &sail_build)?;
    }

    // 3) Build required targets
    if std::env::var("TIR_SKIP_SAIL_FETCH").ok().as_deref() != Some("1") {
        cmake_build(sh, &sail_build, "build_rocq_rv32d")?;
        cmake_build(sh, &sail_build, "build_rocq_rv64d")?;
    }

    // 4) Generate Rocq from our TMDL for RISC-V
    let out = root.join("target/verify/rocq/riscv.v");
    emit_rocq_riscv(sh, &out)?;

    // 5) Generate proof files for RV32 and RV64 (phase 2)
    let proof32 = root.join("target/verify/rocq/proofs_rv32.v");
    let proof64 = root.join("target/verify/rocq/proofs_rv64.v");
    // Use CLI parameters for Sail namespace/module and XLEN definition
    cmd!(sh, "cargo run -p tmdl --bin tmdlc -- --action emit-rocq-sail-proof --dialect riscv --output {proof32} --sail-namespace Riscv --sail-module rv32d --define XLEN=32 {root}/backends/riscv/defs/main.tmdl {root}/backends/riscv/defs/base.tmdl").run()?;
    cmd!(sh, "cargo run -p tmdl --bin tmdlc -- --action emit-rocq-sail-proof --dialect riscv --output {proof64} --sail-namespace Riscv --sail-module rv64d --define XLEN=64 {root}/backends/riscv/defs/main.tmdl {root}/backends/riscv/defs/base.tmdl").run()?;

    // 6) Precompile TMDL and Sail modules, then typecheck proofs with rocq.
    let rocq_dir = root.join("target/verify/rocq");
    let sail_rocq_dir = root.join("target/verify/_deps/sail-riscv/build/rocq");
    // Precompile TMDL module (riscv.v)
    let riscv_mod = rocq_dir.join("riscv.v");
    let _ = cmd!(sh, "rocq compile -q -Q {rocq_dir} TMDL {riscv_mod}").run();
    let sail_stdpp_dir = super::utils::find_stdpp_dir(&sail_rocq_dir);

    // Finally, compile the proof files which depend on both namespaces
    match &sail_stdpp_dir {
        Some(stdpp) => {
            let _ = cmd!(sh, "rocq compile -q -Q {rocq_dir} TMDL -Q {sail_rocq_dir} Riscv -Q {stdpp} SailStdpp {proof32}").run();
            let _ = cmd!(sh, "rocq compile -q -Q {rocq_dir} TMDL -Q {sail_rocq_dir} Riscv -Q {stdpp} SailStdpp {proof64}").run();
        }
        None => {
            let _ = cmd!(
                sh,
                "rocq compile -q -Q {rocq_dir} TMDL -Q {sail_rocq_dir} Riscv {proof32}"
            )
            .run();
            let _ = cmd!(
                sh,
                "rocq compile -q -Q {rocq_dir} TMDL -Q {sail_rocq_dir} Riscv {proof64}"
            )
            .run();
        }
    }

    Ok(())
}
