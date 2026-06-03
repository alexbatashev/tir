//! `tir-sched` — a tiny static instruction-schedule analyzer, in the spirit of
//! `llvm-mca`. It does not execute anything: it parses an assembly snippet, looks
//! up each instruction's static scheduling class from a TMDL-generated
//! [`MachineModel`], and prints the per-instruction timing, a resource-pressure
//! summary, and a dependency critical path (which applies the machine's forwarding
//! network). Purely for inspecting and debugging the performance model.

use std::collections::BTreeMap;

use clap::Parser;
use tir_be_common::liveness::{RegRef, op_regs};
use tir_be_common::sched::{InstrSchedClass, MachineModel, Protection};
use tir_be_common::{AsmDialect, MachineInstruction};
use tir_riscv::RiscvDialect;
use tir_sim::ProgramBuilder;

#[derive(Parser)]
#[command(about = "Static instruction-schedule analyzer (no execution)")]
struct Cli {
    /// Machine model to analyze against: `in-order` or `ooo`.
    #[arg(long, default_value = "ooo")]
    machine: String,
    /// Base address for the program image.
    #[arg(long, default_value_t = 0x8000_0000_u64)]
    base_address: u64,
    /// Assembly file to analyze.
    program: String,
}

/// One analyzed instruction: its scheduling class plus the physical registers it
/// reads and writes (used to build the dependency critical path).
struct Inst {
    mnemonic: String,
    class: InstrSchedClass,
    defs: Vec<(String, u16)>,
    uses: Vec<(String, u16)>,
}

fn select_machine(name: &str) -> Option<MachineModel> {
    match name {
        "in-order" | "inorder" => Some(tir_riscv::in_order_core_model()),
        "ooo" | "out-of-order" => Some(tir_riscv::out_of_order_core_model()),
        _ => None,
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

fn main() {
    let args = Cli::parse();

    let model = select_machine(&args.machine).unwrap_or_else(|| {
        eprintln!(
            "unknown machine '{}' (expected: in-order, ooo)",
            args.machine
        );
        std::process::exit(2);
    });

    let src = std::fs::read_to_string(&args.program).expect("failed to read program file");

    let context = tir::Context::with_default_dialects();
    context.register_dialect::<AsmDialect>();
    context.register_dialect::<RiscvDialect>();
    let dialect = context
        .find_dialect::<RiscvDialect>()
        .expect("failed to find riscv dialect");
    let module = dialect
        .get_asm_parser()
        .parse_asm(&context, &src)
        .expect("failed to parse assembly");
    let program = ProgramBuilder::from_module(&context, module, args.base_address, None)
        .expect("failed to build program image");

    // Flatten every machine instruction into one block — like llvm-mca, the input
    // snippet is treated as a single straight-line region / loop body.
    let mut insts: Vec<Inst> = Vec::new();
    for block in &program.blocks {
        for id in &block.instructions {
            let op = context.get_op(*id);
            if let Some(mi) = op.clone().as_interface::<dyn MachineInstruction>() {
                let mnemonic = mi.mnemonic().to_string();
                let class = model.sched_class(&mnemonic);
                let regs = op_regs(&op);
                insts.push(Inst {
                    mnemonic,
                    class,
                    defs: phys_regs(&regs.defs),
                    uses: phys_regs(&regs.uses),
                });
            }
        }
    }

    print_report(&model, &insts);
}

/// The producer→consumer latency between two dependent instructions, honoring the
/// machine's forwarding network (bypass between the producer's and consumer's
/// resources) and falling back to the producer's default latency.
fn edge_latency(model: &MachineModel, producer: &Inst, consumer: &Inst) -> u32 {
    let p_res = producer.class.resources.first().copied();
    let c_res = consumer.class.resources.first().copied();
    if let (Some(p), Some(c)) = (p_res, c_res) {
        if let Some(f) = model.forward_latency(p, c) {
            return u32::from(f);
        }
    }
    u32::from(producer.class.latency)
}

/// Longest dependency chain through the block, in cycles, applying forwarding.
fn critical_path(model: &MachineModel, insts: &[Inst]) -> u32 {
    let mut issue = vec![0u32; insts.len()];
    for i in 0..insts.len() {
        let mut t = 0u32;
        for u in &insts[i].uses {
            // Most recent prior writer of this register is the producer.
            if let Some(j) = (0..i).rev().find(|&j| insts[j].defs.iter().any(|d| d == u)) {
                t = t.max(issue[j] + edge_latency(model, &insts[j], &insts[i]));
            }
        }
        issue[i] = t;
    }
    (0..insts.len())
        .map(|i| issue[i] + u32::from(insts[i].class.latency))
        .max()
        .unwrap_or(0)
}

fn print_report(model: &MachineModel, insts: &[Inst]) {
    println!("Machine: {} (issue width {})", model.name, model.issue_width);
    if !model.pipeline.is_empty() {
        let stages: Vec<String> = model
            .pipeline
            .iter()
            .map(|p| match p.protection {
                Protection::Protected => p.name.to_string(),
                Protection::Unprotected => format!("{}[unprotected]", p.name),
                Protection::Hard => format!("{}[hard]", p.name),
            })
            .collect();
        println!("Pipeline: {}", stages.join(" "));
    }
    if !model.forwards.is_empty() {
        let fwds: Vec<String> = model
            .forwards
            .iter()
            .map(|f| format!("{}->{}={}", f.from, f.to, f.latency))
            .collect();
        println!("Forwards: {}", fwds.join("  "));
    }
    println!();

    // Per-instruction static schedule.
    println!("Instructions ({}):", insts.len());
    for (i, inst) in insts.iter().enumerate() {
        let c = &inst.class;
        let res = if c.resources.is_empty() {
            "-".to_string()
        } else {
            c.resources.join(",")
        };
        println!(
            "  [{i:>3}]  {:<8}  lat={:<2} read@{} write@{}  rthru={}  {}",
            inst.mnemonic,
            c.latency,
            c.read_cycle,
            c.write_cycle(),
            c.rthroughput,
            res,
        );
    }
    println!();

    // Resource pressure per iteration: each instruction occupies each resource it
    // uses for `rthroughput` cycles; pressure is demand divided by available units.
    let mut demand: BTreeMap<&str, f64> = BTreeMap::new();
    for inst in insts {
        let occupancy = f64::from(inst.class.rthroughput.max(1));
        for r in inst.class.resources {
            *demand.entry(*r).or_default() += occupancy;
        }
    }

    let uops = insts.len() as f64;
    let issue_bound = if model.issue_width == 0 {
        uops
    } else {
        uops / f64::from(model.issue_width)
    };

    println!("Resource pressure per iteration:");
    let mut max_pressure = 0.0_f64;
    let mut bottleneck = String::from("issue");
    for r in model.resources {
        let pressure = demand.get(r.name).copied().unwrap_or(0.0) / f64::from(r.units.max(1));
        if pressure > max_pressure {
            max_pressure = pressure;
            bottleneck = r.name.to_string();
        }
        println!(
            "  {:<6} {pressure:>5.2}  ({} unit{})",
            r.name,
            r.units,
            if r.units == 1 { "" } else { "s" },
        );
    }
    println!();

    let block_rthroughput = issue_bound.max(max_pressure);
    if issue_bound >= max_pressure {
        bottleneck = String::from("issue");
    }
    let critical = critical_path(model, insts);
    println!("uOps: {}   Issue bound: {issue_bound:.2} cycles", insts.len());
    println!(
        "Block RThroughput: {block_rthroughput:.2} cycles/iter  (bottleneck: {bottleneck})"
    );
    println!("Critical path (latency, forwarded): {critical} cycles");
}
