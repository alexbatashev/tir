use std::fs;
use std::process::Command;

#[test]
fn report_rejects_a_wrong_result_bit() {
    let directory = tempfile::tempdir().unwrap();
    let report = directory.path().join("report.json");
    fs::write(
        &report,
        r#"{
  "schema_version": 1,
  "profile": "gcc-15.2",
  "generated_at_unix_seconds": 0,
  "host": {
    "target": "x86_64-linux-gnu",
    "library": "glibc 2.43"
  },
  "results": [
    {
      "case_id": "fma.binary64.value.separate",
      "stage": "reference",
      "compiler": {
        "version": "gcc 15.2.0",
        "executable": "/usr/bin/gcc"
      },
      "source_digest": "sha256:test",
      "commands": [["gcc", "probe.c"]],
      "exit_status": 0,
      "observation": {
        "kind": "exact_bits",
        "bits": "0x8000000000000000",
        "flags": []
      },
      "status": "fail",
      "detail": "expected 0x0000000000000000, observed 0x8000000000000000"
    }
  ]
}"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["fp-check", "report", report.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("fail=1"),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
