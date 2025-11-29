use std::path::PathBuf;
use xshell::Shell;

use super::utils::{cmake_build, cmake_configure, git_checkout, project_root};

pub fn verify_riscv(sh: &Shell) -> anyhow::Result<()> {
    git_checkout(
        sh,
        "https://github.com/riscv/sail-riscv.git",
        "0.9",
        "verify/_deps/sail-riscv",
    )?;

    let root = project_root();
    let sail_src: PathBuf = root.join("target/verify/_deps/sail-riscv");
    let sail_build: PathBuf = sail_src.join("build");
    cmake_configure(sh, &sail_src, &sail_build)?;

    // cmake_build(sh, &sail_build, "build_rocq_rv32d")?;
    cmake_build(sh, &sail_build, "build_rocq_rv64d")?;

    let rocq_out = root.join("target/verify/rocq");
    std::fs::remove_dir_all(&rocq_out)?;
    std::fs::create_dir_all(&rocq_out)?;
    sh.change_dir(&rocq_out);
    Ok(())
}
