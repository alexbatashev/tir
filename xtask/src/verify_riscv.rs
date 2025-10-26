use std::path::PathBuf;
use xshell::{cmd, Shell};

use super::utils::{cmake_build, cmake_configure, git_checkout, project_root};

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

    // 3) Build Lean model targets for RISC-V
    if std::env::var("TIR_SKIP_SAIL_FETCH").ok().as_deref() != Some("1") {
        cmake_build(sh, &sail_build, "generated_lean_rv64d")?;
        // Some versions may not define executable targets; attempt but ignore failure.
        let _ = cmd!(sh, "cmake --build {sail_build} --target generated_executable_lean_rv64d --config Release -- -j").run();
        // Optionally also build RV32 if needed later (best-effort)
        let _ = cmd!(sh, "cmake --build {sail_build} --target generated_lean_rv32d --config Release -- -j").run();
        let _ = cmd!(sh, "cmake --build {sail_build} --target generated_executable_lean_rv32d --config Release -- -j").run();
    }

    // 4) Generate Lean files from TMDL
    let lean_out = root.join("target/verify/lean");
    if lean_out.exists() {
        // Clean up if it's a file from previous runs
        if lean_out.is_file() { std::fs::remove_file(&lean_out)?; }
    }
    std::fs::create_dir_all(&lean_out)?;
    cmd!(sh, "cargo run -p tmdl --bin tmdlc -- --action emit-lean --dialect riscv --output {lean_out} {root}/backends/riscv/defs/main.tmdl {root}/backends/riscv/defs/base.tmdl").run()?;

    // 5) Type-check/build with Lake (preferred Lean 4 workflow)
    // Write a minimal lakefile for these modules
    let lakefile = r#"import Lake
open Lake DSL

package «tmdl-verify» where
  moreLeanArgs := #[]

lean_lib TMDL
lean_lib TMDL_Adapter
"#;
    std::fs::write(lean_out.join("lakefile.lean"), lakefile)?;
    // Prefer LAKE_TOOL env, else default to `lake`
    let lake_tool = std::env::var("LAKE_TOOL").ok().unwrap_or_else(|| "lake".to_string());
    // Change xshell working directory to the Lean output dir
    let back_to = project_root();
    sh.change_dir(&lean_out);
    // Build the adapter lib; this type-checks both files
    let lake_res = cmd!(sh, "{lake_tool} build TMDL_Adapter").run();
    // Fallback: if Lake is unavailable or fails, try Lean directly
    if lake_res.is_err() {
        // Prefer LEAN_TOOL env, otherwise `lean`
        let lean_tool = std::env::var("LEAN_TOOL").ok().unwrap_or_else(|| "lean".to_string());
        cmd!(sh, "{lean_tool} --root=. TMDL_Adapter.lean").run()?;
    }
    // Restore cwd for subsequent tasks
    sh.change_dir(&back_to);

    // If needed later, we can instantiate SailIFace against Sail's Lean model here.
    Ok(())
}
