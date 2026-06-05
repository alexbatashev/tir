//! tir-mc is an IR to machine code compiler

use std::{error::Error, ffi::OsString};

use clap::{Args, ValueEnum};
use tir::{
    Context, IRFormatter, Operation, PassManager,
    builtin::{FuncOp, ModuleOp},
};
use tir_be_common::TargetMachine;

use crate::common::read_input;

#[derive(Args)]
pub struct ToolArgs {
    /// Target CPU
    #[arg(long)]
    mcpu: Option<String>,
    /// Target architecture
    #[arg(long)]
    march: String,
    /// Optional stage after which pipeline is stopped
    #[arg(value_enum, long)]
    stage: Option<Stage>,
    /// Input TIR file, or `-`/omitted for stdin.
    input: Option<OsString>,
    /// Input kind: TIR or assembly
    kind: Option<InputKind>,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
#[value(rename_all = "lower")]
pub enum Stage {
    /// Emit IR after instruction selection stage
    ISel,
    /// Emit IR after register allocation stage
    RegAlloc,
}

#[derive(Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum InputKind {
    #[default]
    Auto,
    TIR,
    Assembly,
}

pub fn run(args: ToolArgs) -> Result<(), Box<dyn Error>> {
    let target = tir_targets::select(&args.march, args.mcpu.as_deref()).ok_or_else(|| {
        format!(
            "unknown target '{}' (supported: {})",
            args.march,
            tir_targets::SUPPORTED_TARGETS.join(", ")
        )
    })?;

    let context = Context::with_default_dialects();
    target.register_dialects(&context);

    let stop_after = args.stage.unwrap_or(Stage::RegAlloc);

    let (module, needs_lowering) = parse_input(&args, &context, target.as_ref())?;

    if needs_lowering {
        let mut pm = create_pass_manager(&stop_after, target.as_ref(), &context);

        pm.run(&context, context.get_op(module.id()))
            .map_err(|e| format!("pass pipeline failed: {e}"))?;
    }

    let mut rendered = String::new();
    let mut fmt = IRFormatter::new(&mut rendered);
    module
        .print(&mut fmt)
        .map_err(|e| format!("failed to print IR: {e}"))?;
    print!("{rendered}");

    Ok(())
}

fn parse_input(
    args: &ToolArgs,
    context: &Context,
    target: &dyn TargetMachine,
) -> Result<(ModuleOp, bool), Box<dyn Error>> {
    let input = read_input(args.input.as_ref())?;

    let ty = match args.kind.unwrap_or_default() {
        InputKind::Auto => {
            if let Some(inp) = args.input.as_ref().and_then(|i| i.to_str())
                && (inp.ends_with(".S") || inp.ends_with(".s") || inp.ends_with(".asm"))
            {
                InputKind::Assembly
            } else {
                InputKind::TIR
            }
        }
        _ => args.kind.unwrap_or_default(),
    };

    match ty {
        InputKind::Assembly => Ok((
            target
                .asm_parser(context)
                .parse_asm(context, &input)
                .map_err(|_| "failed to parse assembly input")?,
            false,
        )),
        InputKind::TIR => Ok((
            tir::parse::ir::parse_ir::<ModuleOp>(context, &input).map_err(|(span, err)| {
                format!("failed to parse input at byte {}: {err:?}", span.0)
            })?,
            true,
        )),
        _ => unreachable!(),
    }
}

fn create_pass_manager(
    stage: &Stage,
    target: &dyn TargetMachine,
    context: &Context,
) -> PassManager {
    let mut pm = PassManager::new();

    pm.nest(FuncOp::name()).add_pass(target.isel_pass(context));

    if stage == &Stage::ISel {
        return pm;
    }

    pm.add_pass(target.regalloc_pass());
    pm
}
