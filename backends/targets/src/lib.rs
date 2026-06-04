//! Target registry shared by `tir-mc` and `isasim`.
//!
//! [`select`] maps a `--march` (and optional `--mcpu`) string onto a concrete
//! [`TargetMachine`]. Both the codegen driver and the simulator go through this
//! one entry point, so the set of supported targets — and the spelling of their
//! names — is defined in a single place.

use tir::Context;
use tir_be_common::AsmDialect;
use tir_be_common::AsmParser;
use tir_be_common::TargetMachine;
use tir_be_common::isel::InstructionSelectPass;
use tir_be_common::regalloc::{RegisterAllocationPass, RegisterInfo, TargetRegAlloc};
use tir_be_common::sched::MachineModel;

use arm64::Arm64Dialect;
use tir_riscv::RiscvDialect;

/// Resolve a `--march`/`--mcpu` pair to a target, or `None` if unknown.
///
/// `mcpu` is accepted for forward compatibility (sub-architecture / scheduling
/// model selection) but does not yet influence the choice of backend.
pub fn select(march: &str, _mcpu: Option<&str>) -> Option<Box<dyn TargetMachine>> {
    match march.to_ascii_lowercase().as_str() {
        "riscv" | "riscv64" | "rv64" | "rv64gc" | "rv64i" => Some(Box::new(RiscvTarget)),
        "arm64" | "aarch64" | "armv8" | "armv8a" => Some(Box::new(Arm64Target)),
        _ => None,
    }
}

/// Names accepted by [`select`], one canonical spelling per target, for help
/// text and error messages.
pub const SUPPORTED_TARGETS: &[&str] = &["riscv64", "arm64"];

/// The RISC-V (RV64I) target.
pub struct RiscvTarget;

impl TargetMachine for RiscvTarget {
    fn name(&self) -> &'static str {
        "riscv64"
    }

    fn register_dialects(&self, context: &Context) {
        context.register_dialect::<AsmDialect>();
        context.register_dialect::<RiscvDialect>();
    }

    fn isel_pass(&self, context: &Context) -> InstructionSelectPass {
        tir_riscv::create_isel_pass(context)
    }

    fn regalloc_pass(&self) -> RegisterAllocationPass {
        tir_riscv::create_regalloc_pass()
    }

    fn register_info(&self) -> RegisterInfo {
        tir_riscv::RiscvRegAlloc.register_info()
    }

    fn asm_parser(&self, context: &Context) -> AsmParser {
        context
            .find_dialect::<RiscvDialect>()
            .expect("riscv dialect must be registered before building an asm parser")
            .get_asm_parser()
    }

    fn machine_model(&self, name: &str) -> Option<MachineModel> {
        match name {
            "in-order" | "inorder" => Some(tir_riscv::in_order_core_model()),
            "ooo" | "out-of-order" => Some(tir_riscv::out_of_order_core_model()),
            _ => None,
        }
    }
}

/// The AArch64 (ARMv8-A) target.
pub struct Arm64Target;

impl TargetMachine for Arm64Target {
    fn name(&self) -> &'static str {
        "arm64"
    }

    fn register_dialects(&self, context: &Context) {
        context.register_dialect::<AsmDialect>();
        context.register_dialect::<Arm64Dialect>();
    }

    fn isel_pass(&self, context: &Context) -> InstructionSelectPass {
        arm64::create_isel_pass(context)
    }

    fn regalloc_pass(&self) -> RegisterAllocationPass {
        arm64::create_regalloc_pass()
    }

    fn register_info(&self) -> RegisterInfo {
        arm64::Arm64RegAlloc.register_info()
    }

    fn asm_parser(&self, context: &Context) -> AsmParser {
        context
            .find_dialect::<Arm64Dialect>()
            .expect("arm64 dialect must be registered before building an asm parser")
            .get_asm_parser()
    }

    fn machine_model(&self, name: &str) -> Option<MachineModel> {
        match name {
            "in-order" | "inorder" => Some(arm64::in_order_core_model()),
            "ooo" | "out-of-order" => Some(arm64::out_of_order_core_model()),
            _ => None,
        }
    }
}
