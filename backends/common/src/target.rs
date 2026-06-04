//! Target abstraction shared by the codegen and simulation tools.
//!
//! A [`TargetMachine`] bundles everything a backend exposes behind a single
//! object: dialect registration, the instruction-selection and register
//! allocation passes, an assembly parser and the cycle-approximate machine
//! models. The concrete implementations live in the `tir-targets` crate (which
//! can depend on the individual backend crates); this trait lives in
//! `backends/common` so the passes and parser types are in scope without a
//! dependency cycle.

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
