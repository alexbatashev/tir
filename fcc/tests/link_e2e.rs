//! End-to-end linking: compile a libc-free program with `fcc`, link it via the
//! system `cc`, run it, and check its exit status. LIT cannot express this
//! (no `%t`, no way to execute a produced file), so it lives here. Gated to
//! supported host backends and skipped when `cc` is unavailable.

#![cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]

use std::path::Path;
use std::process::Command;

const FCC: &str = env!("CARGO_BIN_EXE_fcc");
const SOURCE: &str = "int main(void) { return 42; }\n";

fn cc_available() -> bool {
    Command::new("cc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_fcc(dir: &Path, args: &[&str]) {
    let status = Command::new(FCC)
        .args(args)
        .current_dir(dir)
        .status()
        .expect("spawn fcc");
    assert!(status.success(), "fcc {args:?} failed");
}

fn exit_code(dir: &Path, program: &str) -> i32 {
    Command::new(dir.join(program))
        .status()
        .expect("run linked program")
        .code()
        .expect("program exited via signal")
}

#[test]
fn compile_and_link_in_one_step() {
    if !cc_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("r.c"), SOURCE).unwrap();
    run_fcc(dir.path(), &["cc", "r.c", "-o", "r"]);
    assert_eq!(exit_code(dir.path(), "r"), 42);
}

#[test]
fn separate_compile_then_link() {
    if !cc_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("r.c"), SOURCE).unwrap();
    run_fcc(dir.path(), &["cc", "-c", "r.c"]);
    assert!(dir.path().join("r.o").exists(), "r.o was not produced");
    run_fcc(dir.path(), &["cc", "r.o", "-o", "r2"]);
    assert_eq!(exit_code(dir.path(), "r2"), 42);
}
