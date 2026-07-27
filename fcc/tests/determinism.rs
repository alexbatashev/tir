//! The compiler must be reproducible: the same input compiled by separate
//! processes must produce byte-identical assembly. Each process starts with a
//! different random hash state, so a single run cannot catch an ordering that
//! leaks that state into the output — only repeated processes can.

use std::path::PathBuf;
use std::process::Command;

fn compile(source: &PathBuf) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_fcc"))
        .args(["compile", "--march", "x86_64", "--stage", "asm", "-o", "-"])
        .arg(source)
        .output()
        .expect("fcc should run");
    assert!(
        output.status.success(),
        "fcc failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("assembly should be UTF-8")
}

#[test]
fn assembly_is_identical_across_processes() {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("checks/Inputs/determinism.c");
    let expected = compile(&source);
    for _ in 0..8 {
        assert_eq!(
            compile(&source),
            expected,
            "assembly differs between runs of the same input"
        );
    }
}
