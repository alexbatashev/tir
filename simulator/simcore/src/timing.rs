//! A small trace-driven, cycle-stepped timing model — the first piece of the
//! dynamic ("gem5-lite") engine. It does not execute anything itself: it replays a
//! dynamic instruction stream recorded by the functional [`crate::Executor`] (the
//! oracle) and assigns cycles using a TMDL-generated [`MachineModel`].
//!
//! The scoreboard models data dependencies (forwarding-aware), functional-unit
//! contention, issue width, an instruction window (ROB), and in-order vs.
//! out-of-order issue. Branch prediction and caches are intentionally absent here;
//! they arrive later as swappable Rust policies. Because the trace already encodes
//! every taken branch and resolved address, loops and control flow are handled for
//! free.

use std::collections::HashMap;

use tir::{Context, OpId};
use tir_be_common::MachineInstruction;
use tir_be_common::liveness::{RegRef, op_regs};
use tir_be_common::sched::{InstrSchedClass, MachineModel};

/// Knobs the microarchitecture model exposes for experimentation. These are *not*
/// in TMDL by design — sweeping them is the whole point of the Rust engine.
#[derive(Debug, Clone, Copy)]
pub struct TimingConfig {
    /// Issue instructions strictly in program order (in-order core) vs. allow
    /// out-of-order issue bounded only by dependencies, resources, and the window.
    pub in_order: bool,
    /// Maximum in-flight instructions (reorder-buffer size). `0` means unbounded.
    pub window: usize,
}

impl TimingConfig {
    /// A reasonable default derived from the model: a core that declares a `rob`
    /// buffer is treated as out-of-order with that window; otherwise in-order with
    /// an unbounded window (the in-order issue constraint is what serializes it).
    pub fn for_model(model: &MachineModel) -> Self {
        match model.buffer("rob") {
            Some(rob) => Self {
                in_order: false,
                window: rob as usize,
            },
            None => Self {
                in_order: true,
                window: 0,
            },
        }
    }
}

/// The outcome of a timing run.
#[derive(Debug, Clone, Copy)]
pub struct TimingResult {
    pub cycles: u64,
    pub instructions: u64,
}

impl TimingResult {
    /// Instructions retired per cycle.
    pub fn ipc(&self) -> f64 {
        if self.cycles == 0 {
            0.0
        } else {
            self.instructions as f64 / self.cycles as f64
        }
    }
}

/// One instruction in the trace, pre-resolved to its scheduling class and the
/// physical registers it reads/writes.
struct Slot {
    class: InstrSchedClass,
    defs: Vec<(String, u16)>,
    uses: Vec<(String, u16)>,
}

fn phys_regs(refs: &[RegRef]) -> Vec<(String, u16)> {
    refs.iter()
        .filter_map(|r| match r {
            RegRef::Physical { class, index } => Some((class.clone(), *index)),
            RegRef::Virtual { .. } => None,
        })
        .collect()
}

/// The producer→consumer latency between two dependent instructions, honoring the
/// machine's forwarding network and falling back to the producer's latency.
fn edge_latency(model: &MachineModel, producer: &Slot, consumer: &Slot) -> u64 {
    let p = producer.class.resources.first().copied();
    let c = consumer.class.resources.first().copied();
    if let (Some(p), Some(c)) = (p, c) {
        if let Some(f) = model.forward_latency(p, c) {
            return u64::from(f);
        }
    }
    u64::from(producer.class.latency)
}

/// Replay `trace` against `model` and return the cycle count.
pub fn simulate(
    model: &MachineModel,
    context: &Context,
    trace: &[OpId],
    config: &TimingConfig,
) -> TimingResult {
    let slots: Vec<Slot> = trace
        .iter()
        .map(|id| {
            let op = context.get_op(*id);
            let class = op
                .clone()
                .as_interface::<dyn MachineInstruction>()
                .map(|mi| model.sched_class(mi.mnemonic()))
                .unwrap_or(InstrSchedClass::DEFAULT);
            let regs = op_regs(&op);
            Slot {
                class,
                defs: phys_regs(&regs.defs),
                uses: phys_regs(&regs.uses),
            }
        })
        .collect();

    let n = slots.len();
    let width = model.issue_width.max(1) as usize;
    let window = if config.window == 0 {
        usize::MAX
    } else {
        config.window
    };

    // Per-resource "lanes": one free-at-cycle per parallel unit.
    let mut lanes: HashMap<&str, Vec<u64>> = model
        .resources
        .iter()
        .map(|r| (r.name, vec![0u64; r.units.max(1) as usize]))
        .collect();

    let mut dispatch = vec![0u64; n];
    let mut issue = vec![0u64; n];
    let mut retire = vec![0u64; n];
    let mut reg_writer: HashMap<(String, u16), usize> = HashMap::new();

    for i in 0..n {
        // Front end: in-order dispatch, at most `width` per cycle, bounded by the
        // window (can't dispatch until the instruction `window` slots back retires).
        let mut d = if i > 0 { dispatch[i - 1] } else { 0 };
        if i >= width {
            d = d.max(dispatch[i - width] + 1);
        }
        if i >= window {
            d = d.max(retire[i - window]);
        }
        dispatch[i] = d;

        // Operands ready: the latest forwarding-aware producer result.
        let mut operands_ready = 0u64;
        for u in &slots[i].uses {
            if let Some(&j) = reg_writer.get(u) {
                operands_ready =
                    operands_ready.max(issue[j] + edge_latency(model, &slots[j], &slots[i]));
            }
        }

        let mut t = d.max(operands_ready);
        if config.in_order && i > 0 {
            t = t.max(issue[i - 1]);
        }

        // Functional-unit contention: an instruction can't issue until a lane in
        // each resource it needs is free.
        for r in slots[i].class.resources {
            if let Some(lane_set) = lanes.get(*r) {
                t = t.max(lane_set.iter().copied().min().unwrap_or(0));
            }
        }
        issue[i] = t;

        // Reserve the earliest-free lane in each used resource for `rthroughput`.
        let busy_until = t + u64::from(slots[i].class.rthroughput.max(1));
        for r in slots[i].class.resources {
            if let Some(lane_set) = lanes.get_mut(*r) {
                if let Some(lane) = lane_set.iter_mut().min_by_key(|c| **c) {
                    *lane = busy_until;
                }
            }
        }

        for d in &slots[i].defs {
            reg_writer.insert(d.clone(), i);
        }

        // In-order retire: completes at issue + latency, no earlier than its
        // predecessor retires.
        let complete = issue[i] + u64::from(slots[i].class.latency);
        retire[i] = complete.max(if i > 0 { retire[i - 1] } else { 0 });
    }

    let cycles = retire.last().map(|c| c + 1).unwrap_or(0);
    TimingResult {
        cycles,
        instructions: n as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Executor, ProgramBuilder, TraceOptions};
    use tir_be_common::AsmDialect;
    use tir_riscv::RiscvDialect;

    /// Run `asm` functionally, recording the dynamic trace, then time it.
    fn time_asm(asm: &str, model: &MachineModel, config: &TimingConfig) -> TimingResult {
        let context = tir::Context::with_default_dialects();
        context.register_dialect::<AsmDialect>();
        context.register_dialect::<RiscvDialect>();
        let dialect = context.find_dialect::<RiscvDialect>().unwrap();
        let module = dialect.get_asm_parser().parse_asm(&context, asm).unwrap();
        let program = ProgramBuilder::from_module(&context, module, 0x8000_0000, Some("first"))
            .expect("program builder");
        let until_pc = *program.symbols.get("done").unwrap();

        let mut exec = Executor::new(4096);
        exec.enable_trace_recording();
        exec.load(program).unwrap();
        exec.run_with_trace(until_pc, 10_000, TraceOptions::default(), &mut std::io::sink())
            .unwrap();

        simulate(model, &context, exec.trace(), config)
    }

    /// Five independent ALU ops: an out-of-order core overlaps them (wide issue),
    /// an in-order core retires them one per cycle. The engine must reflect that.
    #[test]
    fn ooo_overlaps_independent_work() {
        // The asm parser emits symbols in reverse source order, so the `done`
        // sentinel is declared first to land *after* `first` in memory (giving
        // `first` a fallthrough to stop on).
        let asm = "
            .global done
            done:
              add x0, x0, x0
            .global first
            first:
              add a0, a1, a2
              add a3, a4, a5
              add a6, a7, t0
              add t1, t2, t3
              add t4, t5, t6
        ";

        let in_order_model = tir_riscv::in_order_core_model();
        let ooo_model = tir_riscv::out_of_order_core_model();

        let io = time_asm(asm, &in_order_model, &TimingConfig::for_model(&in_order_model));
        let oo = time_asm(asm, &ooo_model, &TimingConfig::for_model(&ooo_model));

        assert_eq!(io.instructions, 5);
        assert_eq!(oo.instructions, 5);
        // The out-of-order core finishes the independent chain in fewer cycles and
        // sustains higher IPC.
        assert!(oo.cycles < io.cycles, "ooo {} should beat in-order {}", oo.cycles, io.cycles);
        assert!(oo.ipc() > io.ipc());
    }

    /// A dependent chain serializes on both cores regardless of issue width.
    #[test]
    fn dependent_chain_serializes() {
        let asm = "
            .global done
            done:
              add x0, x0, x0
            .global first
            first:
              add a0, a0, a1
              add a0, a0, a1
              add a0, a0, a1
              add a0, a0, a1
        ";
        let ooo_model = tir_riscv::out_of_order_core_model();
        let oo = time_asm(asm, &ooo_model, &TimingConfig::for_model(&ooo_model));
        // Four dependent adds (override latency 2 each) cannot overlap: at least
        // 4 * 2 cycles of dependency latency.
        assert_eq!(oo.instructions, 4);
        assert!(oo.cycles >= 8, "dependent chain too short: {}", oo.cycles);
    }
}
