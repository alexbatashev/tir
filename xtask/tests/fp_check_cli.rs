use std::fs;
use std::process::Command;

fn check_fixture(expectation: &str, observation: &str, reference_status: &str) -> (bool, serde_json::Value) {
    let directory = tempfile::tempdir().unwrap();
    let manifest = directory.path().join("cases.toml");
    let reference = directory.path().join("reference.json");
    let checked = directory.path().join("checked.json");
    fs::write(
        &manifest,
        format!(
            r#"schema_version = 1

[reference]
profile = "gcc-15.2"
compiler_version = "15.2.0"

[[cases]]
id = "fixture.case"
stage = "reference"
source = "probe.c"
language_mode = "c17"
target_requirements = []
compiler_args = []
runtime_input_bits = []
probe = "execute"

[cases.expectation]
{expectation}

[cases.oracle]
kind = "fixture"
identity = "fixture"
version = "1"
reference = "fixture"
"#,
        ),
    )
    .unwrap();
    fs::write(
        &reference,
        format!(
            r#"{{
  "schema_version": 1,
  "profile": "gcc-15.2",
  "generated_at_unix_seconds": 0,
  "host": {{ "target": "x86_64-linux-gnu", "library": "glibc 2.43" }},
  "results": [{{
    "case_id": "fixture.case",
    "stage": "reference",
    "compiler": {{ "version": "15.2.0", "executable": "/usr/bin/gcc" }},
    "source_digest": "sha256:fixture",
    "commands": [["gcc", "probe.c"]],
    "exit_status": 0,
    "observation": {observation},
    "status": "{reference_status}",
    "detail": "fixture"
  }}]
}}"#,
        ),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "fp-check",
            "check",
            "--stage",
            "reference",
            "--manifest",
            manifest.to_str().unwrap(),
            "--reference",
            reference.to_str().unwrap(),
            "--output",
            checked.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let checked = serde_json::from_str(&fs::read_to_string(checked).unwrap()).unwrap();
    (output.status.success(), checked)
}

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
fn check_detects_a_wrong_result_bit() {
    let (success, report) = check_fixture(
        "kind = \"exact_bits\"\nbits = \"0x0000000000000000\"\nflags = []",
        r#"{"kind":"exact_bits","bits":"0x0000000000000001","flags":[]}"#,
        "pass",
    );

    assert!(!success);
    assert_eq!(report["results"][0]["status"], "fail");
    assert_eq!(
        report["results"][0]["detail"],
        "expected bits 0x0000000000000000, observed 0x0000000000000001"
    );
}

#[test]
fn check_detects_a_wrong_zero_sign() {
    let (success, report) = check_fixture(
        "kind = \"exact_bits\"\nbits = \"0x0000000000000000\"\nflags = []",
        r#"{"kind":"exact_bits","bits":"0x8000000000000000","flags":[]}"#,
        "pass",
    );

    assert!(!success);
    assert_eq!(report["results"][0]["status"], "fail");
    assert_eq!(
        report["results"][0]["detail"],
        "expected bits 0x0000000000000000, observed 0x8000000000000000"
    );
}

#[test]
fn check_detects_a_missing_flag() {
    let (success, report) = check_fixture(
        "kind = \"exact_bits\"\nbits = \"0x0000000000000000\"\nflags = [\"inexact\"]",
        r#"{"kind":"exact_bits","bits":"0x0000000000000000","flags":[]}"#,
        "pass",
    );

    assert!(!success);
    assert_eq!(report["results"][0]["status"], "fail");
    assert!(report["results"][0]["detail"]
        .as_str()
        .unwrap()
        .contains("missing flags [\"inexact\"]"));
}

#[test]
fn check_detects_a_missing_errno_update() {
    let (success, report) = check_fixture(
        "kind = \"effects\"\nflags = []\nerrno = 34\nevents = []\ntrapped = false",
        r#"{"kind":"effects","flags":[],"errno":123,"events":[],"trapped":false}"#,
        "pass",
    );

    assert!(!success);
    assert_eq!(report["results"][0]["status"], "fail");
    assert_eq!(
        report["results"][0]["detail"],
        "expected errno Some(34), observed Some(123)"
    );
}

#[test]
fn check_detects_an_unexpected_trap() {
    let (success, report) = check_fixture(
        "kind = \"effects\"\nflags = []\nevents = []\ntrapped = false",
        r#"{"kind":"effects","flags":[],"errno":null,"events":[],"trapped":true}"#,
        "pass",
    );

    assert!(!success);
    assert_eq!(report["results"][0]["status"], "fail");
    assert_eq!(
        report["results"][0]["detail"],
        "expected trapped=false, observed trapped=true"
    );
}

#[test]
fn check_detects_an_absent_instruction() {
    let (success, report) = check_fixture(
        "kind = \"code_shape\"\nrequired = [\"vfmadd\"]\nforbidden = []",
        r#"{"kind":"code_shape","instructions":["vmulsd %xmm1, %xmm0, %xmm0"]}"#,
        "pass",
    );

    assert!(!success);
    assert_eq!(report["results"][0]["status"], "fail");
    assert_eq!(
        report["results"][0]["detail"],
        "required instruction vfmadd is absent"
    );
}

#[test]
fn check_preserves_an_unsupported_capability() {
    let (success, report) = check_fixture(
        "kind = \"exact_bits\"\nbits = \"0x0000000000000000\"\nflags = []",
        "null",
        "unsupported_capability",
    );

    assert!(!success);
    assert_eq!(
        report["results"][0]["status"],
        "unsupported_capability"
    );
    assert_eq!(report["results"][0]["detail"], "fixture");
}

#[test]
fn check_preserves_missing_infrastructure() {
    let (success, report) = check_fixture(
        "kind = \"exact_bits\"\nbits = \"0x0000000000000000\"\nflags = []",
        "null",
        "missing_infrastructure",
    );

    assert!(!success);
    assert_eq!(
        report["results"][0]["status"],
        "missing_infrastructure"
    );
    assert_eq!(report["results"][0]["detail"], "fixture");
}

#[test]
fn check_detects_a_numerical_error_above_the_bound() {
    let (success, report) = check_fixture(
        "kind = \"numerical_bound\"\nmax_error = \"1e-12\"\nmetric = \"relative\"\ndomain = \"[0.5, 2.0]\"\nzero_convention = \"excluded\"\nsubnormal_convention = \"relative\"\nexceptional_values = \"excluded\"",
        r#"{"kind":"numerical_bound","value":"1.0","error":"2e-12"}"#,
        "pass",
    );

    assert!(!success);
    assert_eq!(report["results"][0]["status"], "fail");
    assert_eq!(report["results"][0]["detail"], "error 2e-12 exceeds 1e-12");
}

#[test]
fn reference_preserves_a_malformed_probe() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("probe.c");
    let manifest = directory.path().join("cases.toml");
    let report = directory.path().join("reference.json");
    fs::write(
        &source,
        "#include <stdio.h>\nint main(void) { puts(\"not json\"); return 0; }\n",
    )
    .unwrap();
    fs::write(
        &manifest,
        format!(
            r#"schema_version = 1

[reference]
profile = "gcc-15.2"
compiler_version = "15.2.0"

[[cases]]
id = "malformed.probe"
stage = "reference"
source = "{}"
language_mode = "c17"
target_requirements = []
compiler_args = []
runtime_input_bits = []
probe = "execute"

[cases.expectation]
kind = "exact_bits"
bits = "0x0000000000000000"
flags = []

[cases.oracle]
kind = "fixture"
identity = "fixture"
version = "1"
reference = "fixture"
"#,
            source.display()
        ),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "fp-check",
            "reference",
            "--gcc",
            "gcc",
            "--manifest",
            manifest.to_str().unwrap(),
            "--output",
            report.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(report).unwrap()).unwrap();
    assert_eq!(report["results"][0]["status"], "fail");
    let artifacts = report["results"][0]["artifacts"].as_str().unwrap();
    assert!(std::path::Path::new(artifacts).join("probe.c").is_file());
    fs::remove_dir_all(artifacts).unwrap();
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
