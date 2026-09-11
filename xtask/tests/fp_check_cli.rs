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

#[test]
fn check_accepts_the_fma_reference_bits_and_flags() {
    let directory = tempfile::tempdir().unwrap();
    let reference = directory.path().join("reference.json");
    let output_path = directory.path().join("checked.json");
    fs::write(
        &reference,
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
      "compiler": { "version": "gcc 15.2.0", "executable": "/usr/bin/gcc" },
      "source_digest": "sha256:test",
      "commands": [["gcc", "fma.c", "separate"]],
      "exit_status": 0,
      "observation": { "kind": "exact_bits", "bits": "0x0000000000000000", "flags": ["inexact"] },
      "status": "pass",
      "detail": "matches exact derivation"
    },
    {
      "case_id": "fma.binary64.value.fused",
      "stage": "reference",
      "compiler": { "version": "gcc 15.2.0", "executable": "/usr/bin/gcc" },
      "source_digest": "sha256:test",
      "commands": [["gcc", "fma.c", "fused"]],
      "exit_status": 0,
      "observation": { "kind": "exact_bits", "bits": "0xbc90000000000000", "flags": [] },
      "status": "pass",
      "detail": "matches exact derivation"
    }
  ]
}"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "fp-check",
            "check",
            "--stage",
            "reference",
            "--reference",
            reference.to_str().unwrap(),
            "--output",
            output_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let checked: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(output_path).unwrap()).unwrap();
    assert_eq!(checked["results"][0]["status"], "pass");
    assert_eq!(checked["results"][1]["status"], "pass");
}

#[test]
fn reference_records_gcc_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let report = directory.path().join("reference.json");
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "fp-check",
            "reference",
            "--gcc",
            "gcc",
            "--case",
            "fma.binary64.value.fused",
            "--output",
            report.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(report).unwrap()).unwrap();
    assert_eq!(report["profile"], "gcc-15.2");
    assert_eq!(report["host"]["target"], "x86_64-linux-gnu");
    assert!(report["host"]["library"]
        .as_str()
        .unwrap()
        .starts_with("glibc "));
    assert_eq!(report["results"][0]["compiler"]["version"], "15.2.0");
    assert!(report["results"][0]["compiler"]["executable"]
        .as_str()
        .unwrap()
        .contains("gcc"));
    assert!(report["results"][0]["source_digest"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert!(report["results"][0]["commands"].is_array());
    assert_eq!(
        report["results"][0]["observation"]["bits"],
        "0xbc90000000000000"
    );
    assert_eq!(report["results"][0]["status"], "pass");
}

#[test]
fn reference_rejects_a_missing_compiler() {
    let directory = tempfile::tempdir().unwrap();
    let report = directory.path().join("reference.json");
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "fp-check",
            "reference",
            "--gcc",
            "/definitely/missing/gcc",
            "--output",
            report.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("/definitely/missing/gcc"));
}

#[test]
fn check_rejects_an_empty_case_selection() {
    let directory = tempfile::tempdir().unwrap();
    let reference = directory.path().join("reference.json");
    let checked = directory.path().join("checked.json");
    fs::write(
        &reference,
        r#"{
  "schema_version": 1,
  "profile": "gcc-15.2",
  "generated_at_unix_seconds": 0,
  "host": { "target": "x86_64-linux-gnu", "library": "glibc 2.43" },
  "results": []
}"#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "fp-check",
            "check",
            "--stage",
            "reference",
            "--case",
            "missing.case",
            "--reference",
            reference.to_str().unwrap(),
            "--output",
            checked.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no cases selected"));
    assert!(!checked.exists());
}

#[test]
fn reference_records_same_expression_contraction() {
    let directory = tempfile::tempdir().unwrap();
    let report = directory.path().join("reference.json");
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "fp-check",
            "reference",
            "--gcc",
            "gcc",
            "--case",
            "contraction.gnu17.same.default",
            "--output",
            report.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(report).unwrap()).unwrap();
    assert_eq!(report["results"][0]["status"], "pass");
    assert!(report["results"][0]["observation"]["instructions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|line| line.as_str().unwrap().contains("vfmadd")));
}

#[test]
fn reference_records_underflow_for_minimum_subnormal_scaling() {
    let directory = tempfile::tempdir().unwrap();
    let report = directory.path().join("reference.json");
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "fp-check",
            "reference",
            "--gcc",
            "gcc",
            "--case",
            "scale.binary64.positive_min_subnormal",
            "--output",
            report.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(report).unwrap()).unwrap();
    assert_eq!(report["results"][0]["observation"]["bits"], "0x0000000000000000");
    assert_eq!(
        report["results"][0]["observation"]["flags"],
        serde_json::json!(["underflow", "inexact"])
    );
}

#[test]
fn reference_records_directed_halfway_rounding() {
    let directory = tempfile::tempdir().unwrap();
    let report = directory.path().join("reference.json");
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "fp-check",
            "reference",
            "--gcc",
            "gcc",
            "--case",
            "round.binary64.halfway.upward",
            "--output",
            report.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(report).unwrap()).unwrap();
    assert_eq!(report["results"][0]["observation"]["bits"], "0x3ff0000000000001");
    assert_eq!(report["results"][0]["observation"]["flags"], serde_json::json!(["inexact"]));
}

#[test]
fn reference_records_observable_dead_arithmetic() {
    let directory = tempfile::tempdir().unwrap();
    let report = directory.path().join("reference.json");
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "fp-check",
            "reference",
            "--gcc",
            "gcc",
            "--case",
            "effects.dead_division.flags",
            "--output",
            report.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(report).unwrap()).unwrap();
    assert_eq!(report["results"][0]["observation"]["flags"], serde_json::json!([]));
    assert_eq!(report["results"][0]["observation"]["errno"], 123);
    assert_eq!(report["results"][0]["status"], "pass");
}

#[test]
fn reference_records_negative_sqrt_reporting() {
    let directory = tempfile::tempdir().unwrap();
    let report = directory.path().join("reference.json");
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "fp-check",
            "reference",
            "--gcc",
            "gcc",
            "--case",
            "math.sqrt.negative.glibc",
            "--output",
            report.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(report).unwrap()).unwrap();
    assert_eq!(report["results"][0]["observation"]["flags"], serde_json::json!(["invalid"]));
    assert_eq!(report["results"][0]["observation"]["errno"], 33);
    assert_eq!(report["results"][0]["observation"]["result_bits"], "0xfff8000000000000");
}

#[test]
fn reference_records_disabled_builtin_recognition() {
    let directory = tempfile::tempdir().unwrap();
    let report = directory.path().join("reference.json");
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "fp-check",
            "reference",
            "--gcc",
            "gcc",
            "--case",
            "recognition.sqrt.builtin_disabled",
            "--output",
            report.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(report).unwrap()).unwrap();
    assert!(report["results"][0]["observation"]["instructions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|line| line.as_str().unwrap().contains("sqrt@PLT")));
}

#[test]
fn reference_accepts_the_expected_declaration_diagnostic() {
    let directory = tempfile::tempdir().unwrap();
    let report = directory.path().join("reference.json");
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "fp-check",
            "reference",
            "--gcc",
            "gcc",
            "--case",
            "recognition.sqrt.declaration_mismatch",
            "--output",
            report.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(report).unwrap()).unwrap();
    assert_eq!(report["results"][0]["status"], "pass");
    assert!(report["results"][0]["observation"]["message"]
        .as_str()
        .unwrap()
        .contains("conflicting types for"));
}

#[test]
fn reference_records_reserved_cases_as_unsupported() {
    let directory = tempfile::tempdir().unwrap();
    let report = directory.path().join("reference.json");
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "fp-check",
            "reference",
            "--gcc",
            "gcc",
            "--case",
            "vector.sqrt.inactive_lane",
            "--output",
            report.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(report).unwrap()).unwrap();
    assert_eq!(report["results"][0]["status"], "unsupported_capability");
    assert!(report["results"][0]["observation"].is_null());
}
