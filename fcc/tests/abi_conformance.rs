use std::fmt::Write;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const FCC: &str = env!("CARGO_BIN_EXE_fcc");

#[derive(Clone, Copy, Debug)]
enum Target {
    Riscv64,
    Arm64,
}

impl Target {
    fn fcc_march(self) -> &'static str {
        match self {
            Self::Riscv64 => "riscv64",
            Self::Arm64 => "arm64",
        }
    }

    fn fcc_abi(self) -> &'static str {
        match self {
            Self::Riscv64 => "lp64d",
            Self::Arm64 => "aapcs64",
        }
    }

    fn cross_cc(self) -> &'static str {
        match self {
            Self::Riscv64 => "riscv64-linux-gnu-gcc",
            Self::Arm64 => "aarch64-linux-gnu-gcc",
        }
    }

    fn emulator(self) -> &'static str {
        match self {
            Self::Riscv64 => "qemu-riscv64",
            Self::Arm64 => "qemu-aarch64",
        }
    }

    fn cross_cc_flags(self) -> &'static [&'static str] {
        match self {
            Self::Riscv64 => &["-march=rv64gc", "-mabi=lp64d"],
            Self::Arm64 => &[],
        }
    }
}

struct GeneratedSuite {
    caller: String,
    callee: String,
}

#[derive(Clone, Copy, Debug)]
enum CompilerDirection {
    FccCaller,
    FccCallee,
}

#[derive(Clone, Copy)]
enum FieldKind {
    Long,
    Double,
}

impl FieldKind {
    fn c_type(self) -> &'static str {
        match self {
            Self::Long => "long",
            Self::Double => "double",
        }
    }
}

struct Field {
    name: &'static str,
    kind: FieldKind,
    value: &'static str,
}

struct AggregateCase {
    name: &'static str,
    tag: &'static str,
    fields: &'static [Field],
    integer_pressure: usize,
    float_pressure: usize,
}

const INTEGER_PAIR_FIELDS: &[Field] = &[
    Field {
        name: "left",
        kind: FieldKind::Long,
        value: "101",
    },
    Field {
        name: "right",
        kind: FieldKind::Long,
        value: "202",
    },
];
const MIXED_PAIR_FIELDS: &[Field] = &[
    Field {
        name: "fp",
        kind: FieldKind::Double,
        value: "3.5",
    },
    Field {
        name: "integer",
        kind: FieldKind::Long,
        value: "404",
    },
];
const FLOAT_PAIR_FIELDS: &[Field] = &[
    Field {
        name: "left",
        kind: FieldKind::Double,
        value: "1.25",
    },
    Field {
        name: "right",
        kind: FieldKind::Double,
        value: "2.5",
    },
];
const FLOAT_QUAD_FIELDS: &[Field] = &[
    Field {
        name: "first",
        kind: FieldKind::Double,
        value: "4.25",
    },
    Field {
        name: "second",
        kind: FieldKind::Double,
        value: "5.5",
    },
    Field {
        name: "third",
        kind: FieldKind::Double,
        value: "6.75",
    },
    Field {
        name: "fourth",
        kind: FieldKind::Double,
        value: "7.125",
    },
];
const LARGE_RECORD_FIELDS: &[Field] = &[
    Field {
        name: "first",
        kind: FieldKind::Long,
        value: "501",
    },
    Field {
        name: "second",
        kind: FieldKind::Long,
        value: "602",
    },
    Field {
        name: "third",
        kind: FieldKind::Long,
        value: "703",
    },
];
const CASES: &[AggregateCase] = &[
    AggregateCase {
        name: "integer_pair",
        tag: "IntegerPair",
        fields: INTEGER_PAIR_FIELDS,
        integer_pressure: 7,
        float_pressure: 0,
    },
    AggregateCase {
        name: "mixed_pair",
        tag: "MixedPair",
        fields: MIXED_PAIR_FIELDS,
        integer_pressure: 7,
        float_pressure: 7,
    },
    AggregateCase {
        name: "float_pair",
        tag: "FloatPair",
        fields: FLOAT_PAIR_FIELDS,
        integer_pressure: 0,
        float_pressure: 7,
    },
    AggregateCase {
        name: "float_quad",
        tag: "FloatQuad",
        fields: FLOAT_QUAD_FIELDS,
        integer_pressure: 0,
        float_pressure: 6,
    },
    AggregateCase {
        name: "large_record",
        tag: "LargeRecord",
        fields: LARGE_RECORD_FIELDS,
        integer_pressure: 8,
        float_pressure: 0,
    },
];

fn generate_suite(target: Target) -> GeneratedSuite {
    let mut common = String::new();
    for case in CASES {
        write!(&mut common, "struct {} {{", case.tag).unwrap();
        for field in case.fields {
            write!(&mut common, " {} {};", field.kind.c_type(), field.name).unwrap();
        }
        common.push_str(" };\n");
    }
    for case in CASES {
        write!(
            &mut common,
            "long check_{}({});\nstruct {} make_{}(void);\n",
            case.name,
            parameter_declarations(target, case),
            case.tag,
            case.name,
        )
        .unwrap();
    }

    let mut caller = common.clone();
    caller.push_str("int main(void) {\n");
    for (index, case) in CASES.iter().enumerate() {
        writeln!(
            &mut caller,
            "  struct {} value_{} = {{{}}};",
            case.tag,
            case.name,
            field_values(case),
        )
        .unwrap();
        write!(
            &mut caller,
            "  long status_{} = check_{}({});\n  if (status_{} != 0) return {} + status_{};\n",
            case.name,
            case.name,
            call_arguments(target, case, Some(&format!("value_{}", case.name))),
            case.name,
            (index + 1) * 32,
            case.name,
        )
        .unwrap();
        writeln!(
            &mut caller,
            "  struct {} result_{} = make_{}();",
            case.tag, case.name, case.name,
        )
        .unwrap();
        for field in case.fields {
            writeln!(
                &mut caller,
                "  if (result_{}.{} != {}) return {};",
                case.name,
                field.name,
                field.value,
                index + 1,
            )
            .unwrap();
        }
    }
    caller.push_str("  return 0;\n}\n");

    let mut callee = common;
    for case in CASES {
        writeln!(
            &mut callee,
            "long check_{}({}) {{",
            case.name,
            parameter_declarations(target, case),
        )
        .unwrap();
        for index in 0..case.integer_pressure {
            writeln!(
                &mut callee,
                "  if (i{index} != {}) return {};",
                integer_value(index),
                10 + index,
            )
            .unwrap();
        }
        for index in 0..case.float_pressure {
            writeln!(
                &mut callee,
                "  if (d{index} != {}) return {};",
                float_value(index),
                20 + index,
            )
            .unwrap();
        }
        for (index, field) in case.fields.iter().enumerate() {
            writeln!(
                &mut callee,
                "  if (value.{} != {}) return {};",
                field.name,
                field.value,
                30 + index,
            )
            .unwrap();
        }
        callee.push_str("  return 0;\n}\n");
        write!(
            &mut callee,
            "struct {} make_{}(void) {{\n  struct {} result = {{{}}};\n  return result;\n}}\n",
            case.tag,
            case.name,
            case.tag,
            field_values(case),
        )
        .unwrap();
    }

    GeneratedSuite { caller, callee }
}

fn parameter_declarations(_target: Target, case: &AggregateCase) -> String {
    let mut parameters = Vec::new();
    for index in 0..case.integer_pressure {
        parameters.push(format!("long i{index}"));
    }
    for index in 0..case.float_pressure {
        parameters.push(format!("double d{index}"));
    }
    parameters.push(format!("struct {} value", case.tag));
    parameters.join(", ")
}

fn call_arguments(_target: Target, case: &AggregateCase, value: Option<&str>) -> String {
    let mut arguments = Vec::new();
    for index in 0..case.integer_pressure {
        arguments.push(integer_value(index).to_string());
    }
    for index in 0..case.float_pressure {
        arguments.push(float_value(index).to_string());
    }
    if let Some(value) = value {
        arguments.push(value.to_string());
    }
    arguments.join(", ")
}

fn field_values(case: &AggregateCase) -> String {
    case.fields
        .iter()
        .map(|field| field.value)
        .collect::<Vec<_>>()
        .join(", ")
}

fn integer_value(index: usize) -> i64 {
    1000 + index as i64 * 17
}

fn float_value(index: usize) -> String {
    format!("{}.25", 20 + index)
}

fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn run_host_pair(suite: &GeneratedSuite, direction: CompilerDirection) {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("caller.c"), &suite.caller).unwrap();
    fs::write(directory.path().join("callee.c"), &suite.callee).unwrap();

    let (caller_compiler, callee_compiler) = match direction {
        CompilerDirection::FccCaller => (FCC, "cc"),
        CompilerDirection::FccCallee => ("cc", FCC),
    };
    compile_host_object(directory.path(), caller_compiler, "caller.c", "caller.o");
    compile_host_object(directory.path(), callee_compiler, "callee.c", "callee.o");
    assert_success(
        Command::new("cc")
            .args(["caller.o", "callee.o", "-o", "abi-conformance"])
            .current_dir(directory.path())
            .output()
            .expect("spawn host linker"),
        "link generated ABI suite",
    );
    assert_success(
        Command::new(directory.path().join("abi-conformance"))
            .output()
            .expect("run generated ABI suite"),
        &format!("run generated ABI suite with {direction:?}"),
    );
}

fn run_cross_target(target: Target) {
    if !tool_available(target.cross_cc()) {
        require_or_skip(target.cross_cc(), target);
        return;
    }

    let suite = generate_suite(target);
    for direction in [CompilerDirection::FccCaller, CompilerDirection::FccCallee] {
        run_cross_pair(&suite, target, direction);
    }
}

fn run_cross_pair(suite: &GeneratedSuite, target: Target, direction: CompilerDirection) {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("caller.c"), &suite.caller).unwrap();
    fs::write(directory.path().join("callee.c"), &suite.callee).unwrap();

    compile_cross_object(
        directory.path(),
        target,
        matches!(direction, CompilerDirection::FccCaller),
        "caller.c",
        "caller.o",
    );
    compile_cross_object(
        directory.path(),
        target,
        matches!(direction, CompilerDirection::FccCallee),
        "callee.c",
        "callee.o",
    );

    let mut linker = Command::new(target.cross_cc());
    linker.args(target.cross_cc_flags());
    assert_success(
        linker
            .args(["-static", "caller.o", "callee.o", "-o", "abi-conformance"])
            .current_dir(directory.path())
            .output()
            .expect("spawn cross linker"),
        &format!("link generated {target:?} ABI suite"),
    );

    if !tool_available(target.emulator()) {
        require_or_skip(target.emulator(), target);
        return;
    }
    assert_success(
        Command::new(target.emulator())
            .arg(directory.path().join("abi-conformance"))
            .output()
            .expect("run generated cross ABI suite"),
        &format!("run generated {target:?} ABI suite with {direction:?}"),
    );
}

fn compile_cross_object(
    directory: &Path,
    target: Target,
    with_fcc: bool,
    input: &str,
    output: &str,
) {
    let mut command = if with_fcc {
        let mut command = Command::new(FCC);
        command.args([
            "cc",
            "-c",
            &format!("-march={}", target.fcc_march()),
            &format!("-mabi={}", target.fcc_abi()),
        ]);
        command
    } else {
        let mut command = Command::new(target.cross_cc());
        command.args(target.cross_cc_flags());
        command.arg("-c");
        command
    };
    assert_success(
        command
            .args([input, "-o", output])
            .current_dir(directory)
            .output()
            .expect("spawn cross compiler"),
        &format!(
            "compile {input} for {target:?} with {}",
            if with_fcc { "fcc" } else { target.cross_cc() },
        ),
    );
}

fn require_or_skip(tool: &str, target: Target) {
    assert!(
        std::env::var_os("TIR_REQUIRE_CROSS_ABI").is_none(),
        "{tool} is required for the {target:?} ABI conformance test",
    );
    eprintln!("skipping {target:?} ABI execution: {tool} is unavailable");
}

fn compile_host_object(directory: &Path, compiler: &str, input: &str, output: &str) {
    let mut command = Command::new(compiler);
    if compiler == FCC {
        command.arg("cc");
    }
    let result = command
        .args(["-c", input, "-o", output])
        .current_dir(directory)
        .output()
        .unwrap_or_else(|error| panic!("spawn {compiler}: {error}"));
    assert_success(result, &format!("compile {input} with {compiler}"));
}

fn assert_success(output: Output, action: &str) {
    assert!(
        output.status.success(),
        "{action} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn generated_suite_covers_each_aggregate_abi_shape() {
    let suite = generate_suite(Target::Riscv64);

    assert!(suite.callee.contains("check_integer_pair"));
    assert!(suite.caller.contains("check_integer_pair"));
    assert!(suite.caller.contains("check_mixed_pair"));
    assert!(suite.caller.contains("check_float_pair"));
    assert!(suite.caller.contains("check_float_quad"));
    assert!(suite.caller.contains("check_large_record"));
    assert!(suite.caller.contains("make_integer_pair"));
    assert!(suite.caller.contains("make_mixed_pair"));
    assert!(suite.caller.contains("make_float_pair"));
    assert!(suite.caller.contains("make_float_quad"));
    assert!(suite.caller.contains("make_large_record"));
    assert!(suite.caller.contains("long i6"));
    assert!(suite.caller.contains("double d6"));
    assert_eq!(suite.caller.matches("return 0;").count(), 1);
}

#[test]
fn generated_suite_roundtrips_with_host_compiler_in_both_directions() {
    if !tool_available("cc") {
        return;
    }

    let suite = generate_suite(Target::Riscv64);
    run_host_pair(&suite, CompilerDirection::FccCaller);
    run_host_pair(&suite, CompilerDirection::FccCallee);
}

#[test]
fn generated_suite_roundtrips_with_cross_compilers_in_both_directions() {
    for target in [Target::Riscv64, Target::Arm64] {
        run_cross_target(target);
    }
}
