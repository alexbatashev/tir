//! `tir sched` prints the data dependence graph of machine IR: how instructions
//! are ordered with respect to one another by their register dependencies.

use std::{error::Error, ffi::OsString};

use clap::Args;
use tir::{
    Context, IRFormatter, Operation, PassManager,
    builtin::{FuncOp, ModuleOp},
};
use tir_be_common::TargetMachine;
use tir_be_common::ddg::{self, Ddg};
use tir_be_common::sched::MachineModel;

use crate::common::{InputKind, parse_module};

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
    /// Input kind: TIR or assembly
    kind: Option<InputKind>,
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

    let (module, needs_lowering) = parse_module(
        target.as_ref(),
        &context,
        args.input.as_ref(),
        args.kind.unwrap_or_default(),
    )?;

    if needs_lowering {
        let mut pm = lowering_pipeline(target.as_ref(), &context);
        pm.run(&context, context.get_op(module.id()))
            .map_err(|e| format!("pass pipeline failed: {e}"))?;
    }

    let model = args
        .model
        .as_deref()
        .and_then(|name| target.machine_model(name));

    let mut out = String::new();
    print_module(&context, &module, model.as_ref(), &mut out)?;
    print!("{out}");

    Ok(())
}

fn lowering_pipeline(target: &dyn TargetMachine, context: &Context) -> PassManager {
    let mut pm = PassManager::new();
    pm.nest(FuncOp::name()).add_pass(target.isel_pass(context));
    pm.add_pass(target.regalloc_pass());
    pm
}

/// Walk the module, emitting one dependence graph per block that holds machine
/// instructions, labelled by the enclosing `asm.symbol` when there is one.
fn print_module(
    context: &Context,
    module: &ModuleOp,
    model: Option<&MachineModel>,
    out: &mut String,
) -> Result<(), Box<dyn Error>> {
    visit_op(context, context.get_op(module.id()).id, None, model, out)
}

fn visit_op(
    context: &Context,
    op_id: tir::OpId,
    label: Option<&str>,
    model: Option<&MachineModel>,
    out: &mut String,
) -> Result<(), Box<dyn Error>> {
    let op = context.get_op(op_id);
    let label = symbol_name(&op).or_else(|| label.map(str::to_string));

    for &region_id in &op.regions {
        for block in context.get_region(region_id).iter(context.clone()) {
            let block = context.get_block(block.id());
            // Skip structural ops (section/symbol markers, module/region enders);
            // machine instructions live in the target dialect.
            let instrs: Vec<tir::OpId> = block
                .op_ids()
                .into_iter()
                .filter(|&id| {
                    let dialect = context.get_op(id).dialect();
                    dialect != "asm" && dialect != "builtin"
                })
                .collect();

            if !instrs.is_empty() {
                let graph = ddg::build(context, &instrs, model);
                print_ddg(context, &graph, label.as_deref(), out)?;
            }

            for id in block.op_ids() {
                visit_op(context, id, label.as_deref(), model, out)?;
            }
        }
    }

    Ok(())
}

fn print_ddg(
    context: &Context,
    graph: &Ddg,
    label: Option<&str>,
    out: &mut String,
) -> Result<(), Box<dyn Error>> {
    use std::fmt::Write;
    use tir::graph::NodeId;

    let title = label.unwrap_or("<anonymous>");
    writeln!(out, "sched @{title} {{")?;

    for i in 0..graph.len() {
        let node = NodeId::from_index(i);
        let mut line = String::new();
        let mut fmt = IRFormatter::new(&mut line);
        context
            .get_op(graph.op(node))
            .as_dyn_op()
            .print(&mut fmt)
            .map_err(|e| format!("failed to print op: {e}"))?;

        write!(out, "  i{i}  {}", line.trim())?;

        let deps: Vec<String> = graph
            .deps(node)
            .map(|e| {
                format!(
                    "{} i{} {} lat={}",
                    e.kind.as_str(),
                    e.on.index(),
                    e.reg,
                    e.latency
                )
            })
            .collect();
        if !deps.is_empty() {
            write!(out, "  ; deps: {}", deps.join(", "))?;
        }
        writeln!(out)?;
    }

    writeln!(out, "}}")?;
    Ok(())
}

/// The name of an `asm.symbol` op, if `op` is one.
fn symbol_name(op: &tir::OpInstance) -> Option<String> {
    if op.dialect() != "asm" || op.name() != "symbol" {
        return None;
    }
    op.attributes.iter().find_map(|attr| match &attr.value {
        tir::attributes::AttributeValue::Str(s) if attr.name == "name" => Some(s.clone()),
        _ => None,
    })
}
