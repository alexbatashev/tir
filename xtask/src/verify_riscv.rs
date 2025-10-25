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

    // 1b) Checkout AFP (Archive of Formal Proofs) for Word_Lib, needed by LEM session
    // Use GitHub mirror (official): https://github.com/isabelle-prover/afp.git
    // Allow override of ref via AFP_REF env; default to master.
    let afp_ref = std::env::var("AFP_REF").unwrap_or_else(|_| "master".to_string());
    git_checkout(
        sh,
        "https://github.com/isabelle-prover/mirror-afp-2024.git",
        &afp_ref,
        "verify/_deps/afp",
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
        cmake_build(sh, &sail_build, "generated_isabelle_rv32d")?;
        cmake_build(sh, &sail_build, "generated_isabelle_rv64d")?;
    }

    // 4) Generate Isabelle theories from our TMDL for RISC-V (generic, no Sail coupling)
    let isabelle_root = root.join("target/verify/isabelle");
    if !isabelle_root.exists() {
        std::fs::create_dir_all(&isabelle_root)?;
    }
    // Separate directories for stub parent and our TMDL session to avoid directory clashes
    let sail_stub_dir = isabelle_root.join("sail_stub");
    let tmdl_dir = isabelle_root.join("tmdl");
    std::fs::create_dir_all(&sail_stub_dir)?;
    std::fs::create_dir_all(&tmdl_dir)?;

    // Generate TMDL Isabelle artifacts into tmdl_dir
    cmd!(sh, "cargo run -p tmdl --bin tmdlc -- --action emit-isabelle --dialect riscv --output {tmdl_dir} --define XLEN=64 {root}/backends/riscv/defs/main.tmdl {root}/backends/riscv/defs/base.tmdl").run()?;

    // 5) Write adapter and ROOT files
    let adapter_path = tmdl_dir.join("TMDL_Sail_Adapter.thy");
    std::fs::write(&adapter_path, ADAPTER_THY)?;
    let tmdl_root_path = tmdl_dir.join("ROOT");
    std::fs::write(&tmdl_root_path, TMDL_ROOT_FILE)?;
    let sail_root_path = sail_stub_dir.join("ROOT");
    std::fs::write(&sail_root_path, SAIL_ROOT_FILE)?;

    // 6) Run Isabelle build with both directories and Sail RISC-V session.
    // Also include Sail and LEM session roots from OPAM if available.
    let sail_isabelle = sail_build.join("isabelle/rv64d");
    let afp_thys = root.join("target/verify/_deps/afp/thys");
    let opam = std::env::var("OPAM_SWITCH_PREFIX").ok();
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let default_base = std::path::Path::new(&home).join(".opam/default");
    let base = opam
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or(default_base);
    let sail_lib_env = std::env::var("SAIL_ISA_LIB")
        .ok()
        .map(std::path::PathBuf::from);
    let sail_lib = sail_lib_env.unwrap_or_else(|| base.join("share/sail/lib/isabelle"));
    let lem_lib = base.join("share/lem/isabelle-lib");
    // Resolve Isabelle tool
    let isabelle_tool = std::env::var("ISABELLE_TOOL")
        .ok()
        .unwrap_or_else(|| "/home/alex/isabelle/bin/isabelle".to_string());

    // Try to detect AFP Word_Lib root (allow override via AFP_THYS env var)
    let mut afp_dirs: Vec<std::path::PathBuf> = vec![];
    if let Ok(afp_env) = std::env::var("AFP_THYS") {
        let p = std::path::PathBuf::from(afp_env);
        if p.join("Word_Lib").exists() {
            afp_dirs.push(p);
        }
    }
    if afp_dirs.is_empty() {
        let home_path = std::path::Path::new(&home);
        let mut candidates = vec![
            home_path.join("isabelle/AFP/thys"),
            home_path.join("AFP/thys"),
        ];
        // Probe under ~/.isabelle/*/contrib/*/thys
        if let Ok(entries) = std::fs::read_dir(home_path.join(".isabelle")) {
            for e in entries.flatten() {
                let contrib = e.path().join("contrib");
                if let Ok(cent) = std::fs::read_dir(&contrib) {
                    for c in cent.flatten() {
                        let thys = c.path().join("thys");
                        candidates.push(thys);
                    }
                }
            }
        }
        for c in candidates.iter() {
            if c.join("Word_Lib").exists() {
                afp_dirs.push(c.clone());
                break;
            }
        }
    }

    // Build command with available roots, avoiding duplicate LEM sessions
    let use_afp = afp_dirs.get(0).cloned().unwrap_or_else(|| afp_thys.clone());
    cmd!(
        sh,
        "{isabelle_tool} build -d {use_afp} -d {sail_isabelle} -D {tmdl_dir}"
    )
    .run()?;

    Ok(())
}

const ADAPTER_THY: &str = r#"theory TMDL_Sail_Adapter
  imports TMDL_Theorems "Sail-Rv64d.Rv64d"
begin

(* Bind the generic interface points to Sail's public API without naming any instruction constructors. *)

definition exec_decode_sail :: "regstate ⇒ (bitU) list ⇒ (instruction × regstate)" where
  "exec_decode_sail s w = run (ext_decode w) s"

definition exec_run_sail :: "instruction ⇒ regstate ⇒ (ExecutionResult × regstate)" where
  "exec_run_sail i s = run (execute i) s"

(* A state relation will be provided by the user/model; kept abstract here. *)

end
"#;

const SAIL_ROOT_FILE: &str = r#"session "Sail" = "HOL" +
  options [document = false]
"#;

const TMDL_ROOT_FILE: &str = r#"session "TMDL-Riscv" = "Sail-Rv64d" +
  options [document = false]
  sessions "HOL-Library" "Sail" "Word_Lib"
  theories
    TMDL_Core
    TMDL_Theorems
    TMDL_Sail_Corres
    TMDL_Sail_Refinement
    TMDL_Sail_Adapter
"#;
