use std::path::{Path, PathBuf};
use xshell::{cmd, Shell};

pub fn project_root() -> PathBuf {
    let dir = std::env::var("CARGO_MANIFEST_DIR")
        .unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_owned());
    PathBuf::from(dir).parent().unwrap().to_owned()
}

pub fn git_checkout(sh: &Shell, url: &str, tag: &str, dest: &str) -> anyhow::Result<()> {
    let root = project_root();
    let target_dir = root.join("target");
    let dest_dir = target_dir.join(dest);

    // Ensure parent exists
    if let Some(parent) = dest_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if dest_dir.exists() {
        // Fetch and checkout the tag
        cmd!(sh, "git -C {dest_dir} fetch --tags --depth 1").run()?;
        // Try to checkout tag; reset to it
        cmd!(sh, "git -C {dest_dir} checkout {tag}").run()?;
        cmd!(sh, "git -C {dest_dir} reset --hard {tag}").run()?;
    } else {
        // Clone at the requested tag shallowly
        if let Some(parent) = dest_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        cmd!(sh, "git clone --depth 1 --branch {tag} {url} {dest_dir}").run()?;
    }

    Ok(())
}

pub fn cmake_configure(sh: &Shell, source_dir: &Path, build_dir: &Path) -> anyhow::Result<()> {
    if !build_dir.exists() {
        std::fs::create_dir_all(build_dir)?;
    }
    cmd!(sh, "cmake -S {source_dir} -B {build_dir} -DCMAKE_BUILD_TYPE=Release").run()?;
    Ok(())
}

pub fn cmake_build(sh: &Shell, build_dir: &Path, target: &str) -> anyhow::Result<()> {
    cmd!(sh, "cmake --build {build_dir} --target {target} --config Release -- -j").run()?;
    Ok(())
}

pub fn emit_rocq_riscv(sh: &Shell, out_path: &Path) -> anyhow::Result<()> {
    let root = project_root();
    let input1 = root.join("backends/riscv/defs/main.tmdl");
    let input2 = root.join("backends/riscv/defs/base.tmdl");

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Use cargo to run tmdlc to emit Rocq
    let out_str = out_path.to_string_lossy().to_string();
    cmd!(sh, "cargo run -p tmdl --bin tmdlc -- --action emit-rocq --output {out_str} {input1} {input2}")
    .run()?;

    Ok(())
}
