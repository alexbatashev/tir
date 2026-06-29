//! End-to-end test of the `tir-smt` binary over stdin.

use std::io::Write;
use std::process::{Command, Stdio};

fn run(script: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tir-smt"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn tir-smt");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "tir-smt exited with {}", out.status);
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn solves_sat_with_model_from_stdin() {
    let out = run("(declare-const x (_ BitVec 8))\
         (assert (= (bvadd x #x01) #x00))\
         (check-sat)\
         (get-value (x))\n");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "sat");
    assert_eq!(lines[1], "((x #xff))");
}

#[test]
fn reports_unsat() {
    let out = run("(declare-const x (_ BitVec 4))\
         (assert (and (bvult x #x3) (bvugt x #x3)))\
         (check-sat)\n");
    assert_eq!(out.trim(), "unsat");
}
