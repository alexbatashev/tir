//! `tir sched` is a static instruction throughput analyzer, similar to
//! `llvm-mca` or Intel's `IACA`. It prints a rough approximation of
//! instructions going through a device pipeline without actually executing
//! the code.

use std::{error::Error, ffi::OsString};

use clap::Args;
use tir::{
    builtin::{FuncOp, ModuleOp},
    Context, IRFormatter, Operation, PassManager,
};
use tir_be_common::sched::MachineModel;
use tir_be_common::TargetMachine;

use crate::common::{parse_module, InputKind};

mod event;
mod pipeline;

#[derive(Args)]
pub struct ToolArgs {
    /// Target CPU
    #[arg(long)]
    mcpu: Option<String>,
    /// Target architecture
    #[arg(long)]
    march: String,
    /// Performance model used for dependency latencies (e.g. `ooo`, `in-order`).
    #[arg(long)]
    model: Option<String>,
    /// Input TIR file, or `-`/omitted for stdin.
    input: Option<OsString>,
}

pub fn run(args: ToolArgs) -> Result<(), Box<dyn Error>> {
    let target = tir_targets::select(&args.march, args.mcpu.as_deref()).ok_or_else(|| {
        format!(
            "unknown target '{}' (supported: {})",
            args.march,
            tir_targets::supported_targets().join(", ")
        )
    })?;

    let context = Context::with_default_dialects();
    target.register_dialects(&context);

    let (module, _) = parse_module(
        target.as_ref(),
        &context,
        args.input.as_ref(),
        InputKind::Assembly,
    )?;

    let _model = args
        .model
        .as_deref()
        .and_then(|name| target.machine_model(name));

    Ok(())
}
