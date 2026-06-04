//! LIT-style FileCheck tests for target selection through `tir-mc`.

use std::io::Write;
use std::path::PathBuf;

fn main() {
    let tir_mc = cargo_tir_mc_wrapper();
    let tir_mc = tir_mc
        .to_str()
        .expect("tir-mc wrapper path must be valid UTF-8");

    tir_lit::harness_main(env!("CARGO_MANIFEST_DIR"), "checks", &[("tir-mc", tir_mc)]);
}

fn cargo_tir_mc_wrapper() -> PathBuf {
    let mut path = std::env::current_exe().expect("current test executable path");
    path.pop();
    path.push("tir-mc-wrapper.sh");

    let mut file = std::fs::File::create(&path).expect("create tir-mc wrapper");
    writeln!(file, "#!/usr/bin/env bash").expect("write wrapper shebang");
    writeln!(file, "exec cargo run -q -p tir-mc -- \"$@\"").expect("write wrapper body");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&path)
            .expect("wrapper metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("make wrapper executable");
    }

    path
}
