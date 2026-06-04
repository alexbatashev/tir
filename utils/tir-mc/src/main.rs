//! `tir-mc` — an llvm-mc/llc-style driver for the machine backends.
//!
//! It reads textual TIR, selects a target with `--march`/`--mcpu`, runs the
//! requested machine passes (instruction selection and/or register allocation)
//! and prints the resulting machine IR. Pair it with `filecheck` in a `RUN:`
//! line to test isel and regalloc:
//!
//! ```text
//! // RUN: tir-mc --march=riscv64 --run=isel,regalloc %s | filecheck %s
//! ```

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read};

use clap::Parser;
use tir::builtin::{FuncOp, ModuleOp};
use tir::{Context, IRFormatter, Operation, PassManager};

#[derive(Debug, Parser)]
#[command(name = "tir-mc")]
struct Cli {
    /// Target architecture (e.g. `riscv64`, `arm64`).
    #[arg(long)]
    march: String,

    /// Target CPU. Accepted for forward compatibility; currently unused.
    #[arg(long)]
    mcpu: Option<String>,

    /// Comma-separated machine passes to run, in order. Supported: `isel`,
    /// `regalloc`.
    #[arg(long, default_value = "isel,regalloc", value_delimiter = ',')]
    run: Vec<String>,

    /// Stop after this pass (sugar for truncating `--run`). One of `isel`,
    /// `regalloc`.
    #[arg(long)]
    stop_after: Option<String>,

    /// Input TIR file, or `-`/omitted for stdin.
    input: Option<OsString>,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("tir-mc: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();

    let target = tir_targets::select(&cli.march, cli.mcpu.as_deref()).ok_or_else(|| {
        format!(
            "unknown target '{}' (supported: {})",
            cli.march,
            tir_targets::SUPPORTED_TARGETS.join(", ")
        )
    })?;

    let stages = resolve_stages(&cli.run, cli.stop_after.as_deref())?;

    let input = read_input(cli.input.as_ref())?;

    let context = Context::with_default_dialects();
    target.register_dialects(&context);

    let module = tir::parse::ir::parse_ir::<ModuleOp>(&context, &input)
        .map_err(|(span, err)| format!("failed to parse input at byte {}: {err:?}", span.0))?;

    let mut pm = PassManager::new();
    for stage in &stages {
        match stage.as_str() {
            "isel" => {
                pm.nest(FuncOp::name()).add_pass(target.isel_pass(&context));
            }
            "regalloc" => {
                pm.add_pass(target.regalloc_pass());
            }
            other => return Err(format!("unknown pass '{other}' (supported: isel, regalloc)")),
        }
    }

    pm.run(&context, context.get_op(module.id()))
        .map_err(|e| format!("pass pipeline failed: {e}"))?;

    let mut rendered = String::new();
    let mut fmt = IRFormatter::new(&mut rendered);
    module
        .print(&mut fmt)
        .map_err(|e| format!("failed to print IR: {e}"))?;
    print!("{rendered}");
    Ok(())
}

/// Resolve the ordered list of passes from `--run` and an optional
/// `--stop-after` truncation point.
fn resolve_stages(run: &[String], stop_after: Option<&str>) -> Result<Vec<String>, String> {
    let mut stages: Vec<String> = run
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if let Some(stop) = stop_after {
        let idx = stages
            .iter()
            .position(|s| s == stop)
            .ok_or_else(|| format!("--stop-after={stop} names a pass not in --run"))?;
        stages.truncate(idx + 1);
    }

    if stages.is_empty() {
        return Err("no passes to run (empty --run)".to_string());
    }
    Ok(stages)
}

fn read_input(path: Option<&OsString>) -> Result<String, String> {
    let mut input = String::new();
    match path {
        Some(path) if path != "-" => File::open(path)
            .map_err(|e| format!("failed to open '{}': {e}", path.to_string_lossy()))?
            .read_to_string(&mut input)
            .map_err(|e| format!("failed to read '{}': {e}", path.to_string_lossy()))?,
        _ => io::stdin()
            .read_to_string(&mut input)
            .map_err(|e| format!("failed to read stdin: {e}"))?,
    };
    Ok(input)
}
