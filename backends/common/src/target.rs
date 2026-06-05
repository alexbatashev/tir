//! Target abstraction shared by the codegen and simulation tools.
//!
//! A [`TargetMachine`] bundles everything a backend exposes behind a single
//! object: dialect registration, the instruction-selection and register
//! allocation passes, an assembly parser and the cycle-approximate machine
//! models. Each backend crate implements this trait and registers itself with
//! the [`TARGETS`] registry via `register_target!`; the `tir-targets` crate
//! links the backends and re-exports the registry's lookup helpers.

use linkme::distributed_slice;
use tir::Context;

use crate::AsmParser;
use crate::isel::InstructionSelectPass;
use crate::regalloc::{RegisterAllocationPass, RegisterInfo};
use crate::sched::MachineModel;

/// A selectable code-generation / simulation target.
///
/// Tools obtain one of these from a registry keyed by `--march`/`--mcpu` and
/// drive it uniformly, so adding a backend is a matter of implementing this
/// trait and registering it.
pub trait TargetMachine {
    /// Canonical target name (e.g. `riscv64`, `arm64`).
    fn name(&self) -> &'static str;

    /// Register the dialects this target needs into `context`.
    ///
    /// Implementations must register the shared `asm` dialect alongside their
    /// own machine dialect so parsing and lowering have everything they need.
    fn register_dialects(&self, context: &Context);

    /// The instruction-selection pass, nested under each function.
    fn isel_pass(&self, context: &Context) -> InstructionSelectPass;

    /// The register-allocation pass, run module-wide after instruction
    /// selection.
    fn regalloc_pass(&self) -> RegisterAllocationPass;

    /// The target's register file description. Beyond register allocation this
    /// also tells the simulator which register classes share a physical file
    /// (e.g. AArch64 `GPR`/`GPRsp`), so a value written through one class is
    /// visible through the other.
    fn register_info(&self) -> RegisterInfo;

    /// An assembly parser for this target's textual `.s`/`.S` syntax.
    fn asm_parser(&self, context: &Context) -> AsmParser;

    /// A cycle-approximate machine model by name (e.g. `in-order`, `ooo`), or
    /// `None` if this target has no model under that name.
    fn machine_model(&self, name: &str) -> Option<MachineModel>;
}

/// A target made selectable by `--march`/`--mcpu`.
///
/// Backends contribute entries with [`register_target!`]; tools resolve targets
/// purely through this registry, so adding a backend never requires touching
/// `tir-mc`, `isasim` or the `tir-targets` aggregator.
pub struct TargetInfo {
    /// Canonical names this backend answers to, for help text and diagnostics.
    pub canonical_names: &'static [&'static str],
    /// Parse a `--march`/`--mcpu` pair, returning a target if this backend owns it.
    pub select: SelectFn,
}

/// Parses a `--march`/`--mcpu` pair into a target, if the backend owns it.
pub type SelectFn = fn(march: &str, mcpu: Option<&str>) -> Option<Box<dyn TargetMachine>>;

/// Link-time registry of every target reachable in the final binary.
#[distributed_slice]
pub static TARGETS: [TargetInfo];

/// Resolve a `--march`/`--mcpu` pair to a target, or `None` if no backend accepts it.
pub fn select_target(march: &str, mcpu: Option<&str>) -> Option<Box<dyn TargetMachine>> {
    TARGETS.iter().find_map(|t| (t.select)(march, mcpu))
}

/// Canonical names accepted by [`select_target`], sorted and de-duplicated.
pub fn supported_targets() -> Vec<&'static str> {
    let mut names: Vec<_> = TARGETS
        .iter()
        .flat_map(|t| t.canonical_names.iter().copied())
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Register a target backend so the tools can select it.
///
/// `select` is a `fn(&str, Option<&str>) -> Option<Box<dyn TargetMachine>>`;
/// `names` lists the canonical spellings shown in help and error text.
#[macro_export]
macro_rules! register_target {
    ($select:path, [$($name:expr),+ $(,)?]) => {
        const _: () = {
            #[$crate::linkme::distributed_slice($crate::TARGETS)]
            #[linkme(crate = $crate::linkme)]
            static REGISTRATION: $crate::TargetInfo = $crate::TargetInfo {
                canonical_names: &[$($name),+],
                select: $select,
            };
        };
    };
}
