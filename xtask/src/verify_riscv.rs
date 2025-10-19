use std::path::PathBuf;
use xshell::Shell;

use super::utils::{cmake_build, cmake_configure, emit_rocq_riscv, git_checkout, project_root};

pub fn verify_riscv(sh: &Shell) -> anyhow::Result<()> {
    // 1) Checkout sail-riscv under target/<dest_dir>
    git_checkout(
        sh,
        "https://github.com/riscv/sail-riscv.git",
        "0.9",
        "verify/_deps/sail-riscv",
    )?;

    // 2) Configure CMake for sail-riscv
    let root = project_root();
    let sail_src: PathBuf = root.join("target/verify/_deps/sail-riscv");
    let sail_build: PathBuf = sail_src.join("build");
    cmake_configure(sh, &sail_src, &sail_build)?;

    // 3) Build required targets
    cmake_build(sh, &sail_build, "generated_rocq_rv32d")?;
    cmake_build(sh, &sail_build, "generated_rocq_rv64d")?;

    // 4) Generate Rocq from our TMDL for RISC-V
    let out = root.join("target/verify/rocq/riscv.v");
    emit_rocq_riscv(sh, &out)?;

    Ok(())
}
