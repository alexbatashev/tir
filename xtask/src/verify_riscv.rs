use std::path::PathBuf;
use xshell::{cmd, Shell};

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

    let lean_out = root.join("target/verify/lean");
    std::fs::remove_dir_all(&lean_out)?;
    std::fs::create_dir_all(&lean_out)?;
    sh.change_dir(&lean_out);
    cmd!(sh, "lake init tmdl-riscv").run()?;
    //     cmd!(sh, "cargo run -p tmdl --bin tmdlc -- --action emit-lean --dialect riscv --output {lean_out} {root}/backends/riscv/defs/main.tmdl {root}/backends/riscv/defs/base.tmdl").run()?;

    //     // 5) Type-check/build with Lake (preferred Lean 4 workflow)
    //     // Write a minimal lakefile for these modules
    //     let lakefile = r#"import Lake
    // open Lake DSL

    // package «tmdl-verify» where
    //   moreLeanArgs := #[]

    // lean_lib TMDL
    // lean_lib TMDL_Adapter
    // lean_lib TMDL_Sail_Instance
    // "#;
    //     std::fs::write(lean_out.join("lakefile.lean"), lakefile)?;
    //     // Prefer LAKE_TOOL env, else default to `lake`
    //     let lake_tool = std::env::var("LAKE_TOOL")
    //         .ok()
    //         .unwrap_or_else(|| "lake".to_string());
    //     // Change xshell working directory to the Lean output dir
    //     let back_to = project_root();
    //     sh.change_dir(&lean_out);
    //     // Build the adapter lib; this type-checks both files
    //     // Enforce no-sorry during verification. Add Sail model path to LEAN_PATH so imports resolve when building the instance.
    //     let sail_model_dir = sail_build.join("model/Lean_RV64D");
    //     let lake_res = cmd!(sh, "env LEAN_PATH={sail_model_dir} {lake_tool} -KleanArgs=-DsorryAbort=true build TMDL_Adapter").run();
    //     // Fallback: if Lake is unavailable or fails, try Lean directly
    //     if lake_res.is_err() {
    //         // Prefer LEAN_TOOL env, otherwise `lean`
    //         let lean_tool = std::env::var("LEAN_TOOL")
    //             .ok()
    //             .unwrap_or_else(|| "lean".to_string());
    //         // Lean fallback with sorryAbort as an option
    //         cmd!(sh, "env LEAN_PATH={sail_model_dir} {lean_tool} -DsorryAbort=true --root=. TMDL_Adapter.lean").run()?;
    //     }
    //     // Also build the generated instance proofs now; this is the actual verification step
    //     let lake_inst = cmd!(sh, "env LEAN_PATH={sail_model_dir} {lake_tool} -KleanArgs=-DsorryAbort=true build TMDL_Sail_Instance").run();
    //     if lake_inst.is_err() {
    //         let lean_tool = std::env::var("LEAN_TOOL")
    //             .ok()
    //             .unwrap_or_else(|| "lean".to_string());
    //         cmd!(sh, "env LEAN_PATH={sail_model_dir} {lean_tool} -DsorryAbort=true --root=. TMDL_Sail_Instance.lean").run()?;
    //     }
    //     // Restore cwd for subsequent tasks
    //     sh.change_dir(&back_to);

    //     // If needed later, we can instantiate SailIFace against Sail's Lean model here.
    Ok(())
}
