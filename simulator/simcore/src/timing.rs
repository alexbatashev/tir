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

use crate::predictor::BranchPredictor;

/// Whether a mnemonic is a conditional branch (RISC-V B-type). The predictor only
/// applies to these. (Belongs on the instruction as a control-flow property
/// eventually; a mnemonic table is fine while RISC-V is the only backend.)
fn is_conditional_branch(mnemonic: &str) -> bool {
    matches!(mnemonic, "beq" | "bne" | "blt" | "bge" | "bltu" | "bgeu")
}

/// Knobs the microarchitecture model exposes for experimentation. These are *not*
/// in TMDL by design — sweeping them is the whole point of the Rust engine.
#[derive(Debug, Clone, Copy)]
pub struct TimingConfig {
    /// Issue instructions strictly in program order (in-order core) vs. allow
    /// out-of-order issue bounded only by dependencies, resources, and the window.
    pub in_order: bool,
    /// Maximum in-flight instructions (reorder-buffer size). `0` means unbounded.
    pub window: usize,
    /// Front-end refetch penalty, in cycles, charged on a branch misprediction.
    pub mispredict_penalty: u64,
}

impl TimingConfig {
    /// A reasonable default derived from the model: a core that declares a `rob`
    /// buffer is treated as out-of-order with that window; otherwise in-order with
    /// an unbounded window (the in-order issue constraint is what serializes it).
    /// The mispredict penalty approximates the front-end refill depth.
    pub fn for_model(model: &MachineModel) -> Self {
        let penalty = if model.pipeline.is_empty() {
            8 // deep out-of-order front end
        } else {
            model.pipeline.len() as u64
        };
        match model.buffer("rob") {
            Some(rob) => Self {
                in_order: false,
                window: rob as usize,
                mispredict_penalty: penalty,
            },
            None => Self {
                in_order: true,
                window: 0,
                mispredict_penalty: penalty,
            },
        }
    }
}

/// The outcome of a timing run.
#[derive(Debug, Clone, Copy)]
pub struct TimingResult {
    pub cycles: u64,
    pub instructions: u64,
    /// Conditional branches whose direction was mispredicted.
    pub mispredicts: u64,
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

/// One instruction in the trace, pre-resolved to its scheduling class, address,
/// width, branch-ness, and the physical registers it reads/writes.
struct Slot {
    class: InstrSchedClass,
    pc: u64,
    width: u64,
    is_branch: bool,
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

/// Replay `trace` (a `(op, pc)` stream) against `model` and return the cycle count.
/// `predictor` supplies branch-direction guesses; mispredictions stall the front
/// end by `config.mispredict_penalty` cycles.
pub fn simulate(
    model: &MachineModel,
    context: &Context,
    trace: &[(OpId, u64)],
    config: &TimingConfig,
    predictor: &mut dyn BranchPredictor,
) -> TimingResult {
    let slots: Vec<Slot> = trace
        .iter()
        .map(|(id, pc)| {
            let op = context.get_op(*id);
            let mi = op.clone().as_interface::<dyn MachineInstruction>();
            let (class, width, is_branch) = match &mi {
                Some(mi) => (
                    model.sched_class(mi.mnemonic()),
                    u64::from(mi.width_bytes()),
                    is_conditional_branch(mi.mnemonic()),
                ),
                None => (InstrSchedClass::DEFAULT, 4, false),
            };
            let regs = op_regs(&op);
            Slot {
                class,
                pc: *pc,
                width,
                is_branch,
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
    // Learned branch targets (a minimal BTB), so a not-taken branch still has a
    // direction to predict against.
    let mut btb: HashMap<u64, u64> = HashMap::new();
    // Earliest cycle the front end may resume after a misprediction redirect.
    let mut redirect: u64 = 0;
    let mut mispredicts: u64 = 0;

    for i in 0..n {
        // Front end: in-order dispatch, at most `width` per cycle, bounded by the
        // window (can't dispatch until the instruction `window` slots back retires)
        // and by any outstanding misprediction redirect.
        let mut d = if i > 0 { dispatch[i - 1] } else { 0 };
        if i >= width {
            d = d.max(dispatch[i - width] + 1);
        }
        if i >= window {
            d = d.max(retire[i - window]);
        }
        d = d.max(redirect);
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

        // Branch resolution: compare the predicted direction to the actual outcome
        // (recovered from the trace), and stall the front end on a mispredict.
        if slots[i].is_branch && i + 1 < n {
            let pc = slots[i].pc;
            let fallthrough = pc.wrapping_add(slots[i].width);
            let next_pc = slots[i + 1].pc;
            let taken = next_pc != fallthrough;
            let target = if taken {
                next_pc
            } else {
                btb.get(&pc).copied().unwrap_or(fallthrough)
            };

            let predicted = predictor.predict(pc, target);
            if predicted != taken {
                mispredicts += 1;
                let resolved = issue[i] + u64::from(slots[i].class.latency);
                redirect = redirect.max(resolved + config.mispredict_penalty);
            }
            if taken {
                btb.insert(pc, next_pc);
            }
            predictor.update(pc, target, taken);
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
        mispredicts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Executor, ProgramBuilder, TraceOptions};
    use tir_be_common::AsmDialect;
    use tir_riscv::RiscvDialect;

    use crate::predictor::AlwaysNotTaken;

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
        exec.run_with_trace(
            until_pc,
            10_000,
            TraceOptions::default(),
            &mut std::io::sink(),
        )
        .unwrap();

        simulate(model, &context, exec.trace(), config, &mut AlwaysNotTaken)
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

        let io = time_asm(
            asm,
            &in_order_model,
            &TimingConfig::for_model(&in_order_model),
        );
        let oo = time_asm(asm, &ooo_model, &TimingConfig::for_model(&ooo_model));

        assert_eq!(io.instructions, 5);
        assert_eq!(oo.instructions, 5);
        // The out-of-order core finishes the independent chain in fewer cycles and
        // sustains higher IPC.
        assert!(
            oo.cycles < io.cycles,
            "ooo {} should beat in-order {}",
            oo.cycles,
            io.cycles
        );
        assert!(oo.ipc() > io.ipc());
    }

    /// The predictor changes the cycle count: a *taken backward* branch (loop
    /// back-edge) is mispredicted by always-not-taken (paying the refetch penalty)
    /// but predicted correctly by BTFN. We parse a real branch op for its registers
    /// and width, then drive a synthetic `(op, pc)` trace whose addresses describe
    /// the back-edge — independent of the functional executor's branch handling.
    #[test]
    fn predictor_changes_mispredicts_on_backward_branch() {
        use crate::predictor::BackwardTaken;
        use tir::OpId;

        let context = tir::Context::with_default_dialects();
        context.register_dialect::<AsmDialect>();
        context.register_dialect::<RiscvDialect>();
        let dialect = context.find_dialect::<RiscvDialect>().unwrap();
        let asm = "
            .global blk
            blk:
              beq a0, a0, 0
              add a1, a2, a3
        ";
        let module = dialect.get_asm_parser().parse_asm(&context, asm).unwrap();
        let program =
            ProgramBuilder::from_module(&context, module, 0x8000_0000, Some("blk")).unwrap();
        let ops: Vec<OpId> = program
            .blocks
            .iter()
            .flat_map(|b| b.instructions.iter().copied())
            .collect();
        assert_eq!(ops.len(), 2);

        // Branch at 0x100 whose successor executes at 0x080: a taken back-edge.
        let trace = vec![(ops[0], 0x100u64), (ops[1], 0x080u64)];
        let model = tir_riscv::out_of_order_core_model();
        let config = TimingConfig::for_model(&model);

        let ant = simulate(&model, &context, &trace, &config, &mut AlwaysNotTaken);
        let btfn = simulate(&model, &context, &trace, &config, &mut BackwardTaken);

        assert_eq!(
            ant.mispredicts, 1,
            "not-taken mispredicts the taken back-edge"
        );
        assert_eq!(btfn.mispredicts, 0, "btfn predicts the back-edge taken");
        assert!(
            ant.cycles > btfn.cycles,
            "misprediction penalty should cost cycles: ant {} vs btfn {}",
            ant.cycles,
            btfn.cycles
        );
    }

    /// End-to-end: a real backward-branch loop runs functionally (3 iterations),
    /// and the recorded trace shows the loop predictor's advantage — always-not-taken
    /// mispredicts every taken back-edge, BTFN only the loop exit.
    #[test]
    fn loop_branch_prediction_end_to_end() {
        use crate::predictor::BackwardTaken;
        use tir_be_common::MachineContext;

        // Sentinel/exit blocks precede the entry so the reverse-ordered layout puts
        // `first` at the base; `bne …, -4` is a single-instruction block, so its PC
        // is exact and it branches back to the decrement.
        let asm = "
            .global done
            done:
              add x0, x0, x0
            .global exitblk
            exitblk:
              add x0, x0, x0
            .global br
            br:
              bne a0, zero, -4
            .global dec
            dec:
              addi a0, a0, -1
            .global first
            first:
              addi a0, zero, 3
        ";
        let context = tir::Context::with_default_dialects();
        context.register_dialect::<AsmDialect>();
        context.register_dialect::<RiscvDialect>();
        let dialect = context.find_dialect::<RiscvDialect>().unwrap();
        let module = dialect.get_asm_parser().parse_asm(&context, asm).unwrap();
        let program =
            ProgramBuilder::from_module(&context, module, 0x8000_0000, Some("first")).unwrap();
        let until_pc = *program.symbols.get("done").unwrap();

        let mut exec = Executor::new(4096);
        exec.enable_trace_recording();
        exec.load(program).unwrap();
        exec.run_with_trace(
            until_pc,
            10_000,
            TraceOptions::default(),
            &mut std::io::sink(),
        )
        .unwrap();

        // The loop ran to completion: counter 3 → 0.
        assert_eq!(
            MachineContext::read_register(&exec, "GPR", 10)
                .unwrap()
                .to_u64(),
            0
        );
        let trace = exec.trace().to_vec();

        let model = tir_riscv::out_of_order_core_model();
        let config = TimingConfig::for_model(&model);
        let ant = simulate(&model, &context, &trace, &config, &mut AlwaysNotTaken);
        let btfn = simulate(&model, &context, &trace, &config, &mut BackwardTaken);

        assert_eq!(
            ant.mispredicts, 2,
            "not-taken mispredicts both taken back-edges"
        );
        assert_eq!(btfn.mispredicts, 1, "btfn only mispredicts the loop exit");
        assert!(
            btfn.cycles < ant.cycles,
            "btfn {} should beat ant {}",
            btfn.cycles,
            ant.cycles
        );
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
