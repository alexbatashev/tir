//! LIT-style FileCheck tests for core IR passes.
//!
//! The tests live with `tir` because they exercise core-library transforms. RUN
//! lines still invoke the `tir-opt` utility so the command-line pass plumbing is
//! covered without making `utils/tir-opt` own core pass test fixtures.

use std::io::Write;
use std::path::PathBuf;

fn main() {
    let tir_opt = cargo_tir_opt_wrapper();
    let tir_opt = tir_opt
        .to_str()
        .expect("tir-opt wrapper path must be valid UTF-8");

    tir_lit::harness_main(
        env!("CARGO_MANIFEST_DIR"),
        "checks",
        &[("tir-opt", tir_opt)],
    );
}

fn cargo_tir_opt_wrapper() -> PathBuf {
    let mut path = std::env::current_exe().expect("current test executable path");
    path.pop();
    path.push("tir-opt-wrapper.sh");

    let mut file = std::fs::File::create(&path).expect("create tir-opt wrapper");
    writeln!(file, "#!/usr/bin/env bash").expect("write wrapper shebang");
    writeln!(file, "exec cargo run -q -p tir-opt -- \"$@\"").expect("write wrapper body");

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
