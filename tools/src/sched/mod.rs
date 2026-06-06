//! `tir sched` is a static instruction throughput analyzer, similar to
//! `llvm-mca` or Intel's `IACA`. It prints a rough approximation of a code
//! region's behavior on a device pipeline without executing it: data
//! dependencies are reconstructed on the fly from each instruction's read/written
//! registers, then dispatch/issue/retire cycles are assigned against a
//! TMDL-generated [`MachineModel`].

use std::collections::{HashMap, HashSet};
use std::{error::Error, ffi::OsString};

use clap::Args;
use tir::Context;
use tir_be_common::liveness::{RegRef, op_regs};
use tir_be_common::{MachineInstruction, SectionOp, SymbolOp};

use crate::common::{InputKind, parse_module};
use crate::sched::event::View;
use crate::sched::pipeline::{BaseInstr, Prf};

mod event;
mod pipeline;

/// The scheduling fallback when no `--model` is selected: a generic single-issue
/// core with no functional units, so every instruction resolves to the
/// single-cycle [`InstrSchedClass::DEFAULT`].
const GENERIC_MODEL: tir_be_common::sched::MachineModel = tir_be_common::sched::MachineModel {
    name: "generic",
    issue_width: 1,
    resources: &[],
    buffers: &[],
    pipeline: &[],
    forwards: &[],
    reg_files: &[],
    sched: &[],
};

#[derive(Args)]
pub struct ToolArgs {
    /// Target CPU
    #[arg(long)]
    mcpu: Option<String>,
    /// Target architecture
    #[arg(long)]
    march: String,
    /// Performance model / machine to analyze against (e.g. `ooo`, `in-order`).
    /// Omitted: a generic single-issue core that costs every instruction one
    /// cycle (the scheduling fallback when no machine is selected).
    #[arg(long)]
    model: Option<String>,
    /// Number of times the region is repeated through the pipeline.
    #[arg(long, default_value_t = 100)]
    iterations: usize,
    /// Report format.
    #[arg(long, value_enum, default_value_t = View::Resource)]
    view: View,
    /// Input assembly file, or `-`/omitted for stdin.
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

    let model = match &args.model {
        Some(name) => target.machine_model(name).ok_or_else(|| {
            format!(
                "unknown machine '{}' for target '{}' (one of: {})",
                name,
                target.name(),
                target.machines().join(", ")
            )
        })?,
        None => GENERIC_MODEL,
    };

    let (module, _) = parse_module(
        target.as_ref(),
        &context,
        args.input.as_ref(),
        InputKind::Assembly,
    )?;

    // Collect the region's machine instructions in program order, resolving each to
    // its scheduling class and the physical registers it reads/writes.
    let mut op_ids = Vec::new();
    collect_instructions(&context, module.body(), &mut op_ids);

    let mut base = Vec::with_capacity(op_ids.len());
    for op_id in op_ids {
        let op = context.get_op(op_id);
        let Some(mi) = op.clone().as_interface::<dyn MachineInstruction>() else {
            continue;
        };
        let mnemonic = mi.mnemonic();
        let regs = op_regs(&op);
        let defs = phys_regs(&regs.defs);
        let uses = phys_regs(&regs.uses);
        base.push(BaseInstr {
            text: render_instruction(mnemonic, &defs, &uses),
            class: model.sched_class(mnemonic),
            defs,
            uses,
        });
    }

    if base.is_empty() {
        return Err("no machine instructions found in input".into());
    }

    let prf = build_prf(&target.register_info(), &model);
    let mut handler = event::make(args.view);
    pipeline::simulate(
        &model,
        &base,
        args.iterations.max(1),
        Some(&prf),
        handler.as_mut(),
    );
    print!("{}", handler.render());

    Ok(())
}

/// Recursively gather the ids of every machine instruction reachable from `block`,
/// in program order, descending through `section`/`symbol` containers.
fn collect_instructions(
    context: &Context,
    block: std::sync::Arc<tir::Block>,
    out: &mut Vec<tir::OpId>,
) {
    for op_id in block.op_ids() {
        let op = context.get_op(op_id);
        if let Some(section) = op.clone().as_op::<SectionOp>() {
            collect_instructions(context, section.body(), out);
        } else if let Some(symbol) = op.clone().as_op::<SymbolOp>() {
            collect_instructions(context, symbol.body(), out);
        } else if op.as_interface::<dyn MachineInstruction>().is_some() {
            out.push(op_id);
        }
    }
}

fn phys_regs(refs: &[RegRef]) -> Vec<(String, u16)> {
    refs.iter()
        .filter_map(|r| match r {
            RegRef::Physical { class, index } => Some((class.clone(), *index)),
            RegRef::Virtual { .. } => None,
        })
        .collect()
}

/// A compact `mnemonic def, ... <- use, ...` rendering for the report. The
/// assembler's exact operand order is not recoverable from the register sets, so
/// definitions and uses are shown grouped rather than in syntactic position.
fn render_instruction(mnemonic: &str, defs: &[(String, u16)], uses: &[(String, u16)]) -> String {
    let fmt = |regs: &[(String, u16)]| {
        regs.iter()
            .map(|(c, i)| format!("{c}{i}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    match (defs.is_empty(), uses.is_empty()) {
        (true, true) => mnemonic.to_string(),
        (false, true) => format!("{mnemonic} {}", fmt(defs)),
        (true, false) => format!("{mnemonic} {}", fmt(uses)),
        (false, false) => format!("{mnemonic} {} <- {}", fmt(defs), fmt(uses)),
    }
}

/// Build the register-file pressure model: map each register class to its physical
/// file and give each file a capacity (the machine's declared `reg_file` count, or
/// the architectural register count of that file as a fallback).
fn build_prf(
    info: &tir_be_common::regalloc::RegisterInfo,
    model: &tir_be_common::sched::MachineModel,
) -> Prf {
    let class_to_file = info
        .classes
        .iter()
        .map(|c| (c.name.to_string(), c.file.to_string()))
        .collect();

    // Architectural register count per file: the number of distinct encoding
    // indices the file's classes name.
    let mut indices: HashMap<&str, HashSet<u16>> = HashMap::new();
    for c in info.classes {
        let set = indices.entry(c.file).or_default();
        for &i in c
            .allocation_order
            .iter()
            .chain(c.reserved)
            .chain(c.caller_saved)
            .chain(c.callee_saved)
            .chain(c.arguments)
            .chain(c.return_values)
        {
            set.insert(i);
        }
    }

    let capacity = indices
        .into_iter()
        .map(|(file, idxs)| {
            let cap = model
                .reg_file(file)
                .unwrap_or_else(|| idxs.len().min(u16::MAX as usize) as u16);
            (file.to_string(), cap)
        })
        .collect();

    Prf {
        class_to_file,
        capacity,
    }
}
