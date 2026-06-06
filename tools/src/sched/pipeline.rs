//! The static throughput engine behind `tir sched`.
//!
//! Unlike the trace-driven timing model in `simcore`, this analyzer never
//! executes anything: it takes the instructions of a code region, repeats them
//! `iterations` times, and reconstructs data dependencies on the fly from each
//! instruction's read/written physical registers (the same renamer idea
//! `llvm-mca` uses, no separate dependence graph). It assigns dispatch/issue/
//! retire cycles honoring data dependencies (forwarding-aware), functional-unit
//! contention, issue width, the reorder-buffer window, in-order vs. out-of-order
//! issue, and — on a renaming core — physical-register-file pressure.
//!
//! The engine is decoupled from presentation: it emits a stream of pipeline events
//! to an [`EventHandler`], and each handler renders a different report (resource
//! utilization, timeline, ...). New views can be added without touching the engine.

use std::collections::{HashMap, VecDeque};

use tir_be_common::sched::{InstrSchedClass, MachineModel};

/// One instruction of the analyzed region, pre-resolved to its scheduling class
/// and the physical registers it reads/writes.
pub struct BaseInstr {
    /// Rendered text for the report (mnemonic + operands).
    pub text: String,
    pub class: InstrSchedClass,
    pub defs: Vec<(String, u16)>,
    pub uses: Vec<(String, u16)>,
}

/// Physical-register-file pressure model for a renaming core.
pub struct Prf {
    /// Register class name -> physical file it draws from.
    pub class_to_file: HashMap<String, String>,
    /// Physical file name -> number of physical registers.
    pub capacity: HashMap<String, u16>,
}

impl Prf {
    fn file_of<'a>(&'a self, class: &'a str) -> &'a str {
        self.class_to_file
            .get(class)
            .map(String::as_str)
            .unwrap_or(class)
    }
}

/// Static context handed to an [`EventHandler`] before the run, so it can size its
/// tables and copy out whatever per-instruction data it needs to report.
pub struct SimContext<'a> {
    pub model: &'a MachineModel,
    pub iterations: usize,
    pub base: &'a [BaseInstr],
}

/// A consumer of pipeline events. Each implementation renders a different report.
/// The instruction index `i` passed to the per-event hooks is the *global* index
/// in the repeated stream; the region instruction is `i % ctx.base.len()` and the
/// iteration is `i / ctx.base.len()`.
pub trait EventHandler {
    fn start(&mut self, _ctx: &SimContext) {}
    fn dispatched(&mut self, _cycle: u64, _i: usize) {}
    fn issued(&mut self, _cycle: u64, _i: usize) {}
    fn retired(&mut self, _cycle: u64, _i: usize) {}
    fn finish(&mut self, _total_cycles: u64) {}
    fn render(&self) -> String;
}

/// The producer->consumer latency between two dependent instructions, honoring the
/// machine's forwarding network and falling back to the producer's latency.
fn edge_latency(
    model: &MachineModel,
    producer: &InstrSchedClass,
    consumer: &InstrSchedClass,
) -> u64 {
    if let (Some(p), Some(c)) = (producer.resources.first(), consumer.resources.first())
        && let Some(f) = model.forward_latency(p, c)
    {
        return u64::from(f);
    }
    u64::from(producer.latency)
}

/// Simulate `base` repeated `iterations` times against `model`, emitting events to
/// `handler`. `prf` enables register-file pressure on a renaming (out-of-order)
/// core; it is ignored for an in-order core, which does not rename.
pub fn simulate(
    model: &MachineModel,
    base: &[BaseInstr],
    iterations: usize,
    prf: Option<&Prf>,
    handler: &mut dyn EventHandler,
) {
    handler.start(&SimContext {
        model,
        iterations,
        base,
    });

    let width = model.issue_width.max(1) as usize;
    // A core with a reorder buffer issues out of order, bounded by that window;
    // otherwise it is in-order with an unbounded window (the in-order constraint is
    // what serializes it). Mirrors `simcore`'s `TimingConfig::for_model`.
    let in_order = model.buffer("rob").is_none();
    let window = model
        .buffer("rob")
        .map(|r| r as usize)
        .unwrap_or(usize::MAX);
    let model_renames = !in_order;

    let n = base.len().saturating_mul(iterations);

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
    // Per physical file, the retire cycles of in-flight register allocations (FIFO:
    // retire times are monotonic, so the oldest allocation frees first).
    let mut prf_inflight: HashMap<String, VecDeque<u64>> = HashMap::new();

    for idx in 0..n {
        let slot = &base[idx % base.len()];

        // Front end: in-order dispatch, at most `width` per cycle, bounded by the
        // reorder-buffer window.
        let mut d = if idx > 0 { dispatch[idx - 1] } else { 0 };
        if idx >= width {
            d = d.max(dispatch[idx - width] + 1);
        }
        if idx >= window {
            d = d.max(retire[idx - window]);
        }

        // Register-file pressure: a renaming core stalls dispatch until enough
        // physical registers free up for this instruction's definitions.
        if model_renames && let Some(prf) = prf {
            let mut need: HashMap<&str, usize> = HashMap::new();
            for (class, _) in &slot.defs {
                *need.entry(prf.file_of(class)).or_default() += 1;
            }
            for (file, need) in need {
                let Some(&cap) = prf.capacity.get(file) else {
                    continue;
                };
                let cap = cap as usize;
                let q = prf_inflight.entry(file.to_string()).or_default();
                // Free registers whose allocating instruction has retired by `d`.
                while q.front().is_some_and(|&c| c <= d) {
                    q.pop_front();
                }
                // If still short, advance dispatch to the retire cycle that frees
                // the needed count (clamped: an instruction needing more registers
                // than the file holds cannot be helped).
                if q.len() + need > cap && cap >= need {
                    let must_free = q.len() + need - cap;
                    if let Some(&free_at) = q.get(must_free - 1) {
                        d = d.max(free_at);
                    }
                    for _ in 0..must_free {
                        q.pop_front();
                    }
                }
            }
        }
        dispatch[idx] = d;
        handler.dispatched(d, idx);

        // Operands ready: the latest forwarding-aware producer result.
        let mut operands_ready = 0u64;
        for u in &slot.uses {
            if let Some(&j) = reg_writer.get(u) {
                let producer = &base[j % base.len()];
                operands_ready = operands_ready
                    .max(issue[j] + edge_latency(model, &producer.class, &slot.class));
            }
        }

        let mut t = d.max(operands_ready);
        if in_order && idx > 0 {
            t = t.max(issue[idx - 1]);
        }

        // Functional-unit contention: wait until a lane in each needed resource is
        // free, then reserve the earliest-free lane for `rthroughput` cycles.
        for r in slot.class.resources {
            if let Some(lane_set) = lanes.get(*r) {
                t = t.max(lane_set.iter().copied().min().unwrap_or(0));
            }
        }
        issue[idx] = t;
        handler.issued(t, idx);

        let busy_until = t + u64::from(slot.class.rthroughput.max(1));
        for r in slot.class.resources {
            if let Some(lane) = lanes
                .get_mut(*r)
                .and_then(|s| s.iter_mut().min_by_key(|c| **c))
            {
                *lane = busy_until;
            }
        }

        for def in &slot.defs {
            reg_writer.insert(def.clone(), idx);
        }

        // In-order retire: completes at issue + latency, no earlier than its
        // predecessor retires.
        let complete = issue[idx] + u64::from(slot.class.latency);
        retire[idx] = complete.max(if idx > 0 { retire[idx - 1] } else { 0 });
        handler.retired(retire[idx], idx);

        if model_renames && let Some(prf) = prf {
            for (class, _) in &slot.defs {
                let file = prf.file_of(class);
                if prf.capacity.contains_key(file) {
                    prf_inflight
                        .entry(file.to_string())
                        .or_default()
                        .push_back(retire[idx]);
                }
            }
        }
    }

    let cycles = retire.last().map(|c| c + 1).unwrap_or(0);
    handler.finish(cycles);
}
