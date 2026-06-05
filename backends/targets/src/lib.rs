//! Target registry shared by `tir-mc` and `isasim`.
//!
//! [`select`] asks each backend to parse the requested `--march`/`--mcpu` pair
//! and wraps the first backend that accepts it as a concrete [`TargetMachine`].
//! Backend-specific spelling rules stay in the backend crates instead of being
//! duplicated in this registry.

use tir::Context;
use tir_be_common::AsmDialect;
use tir_be_common::AsmParser;
use tir_be_common::TargetMachine;
use tir_be_common::isel::InstructionSelectPass;
use tir_be_common::regalloc::{RegisterAllocationPass, RegisterInfo, TargetRegAlloc};
use tir_be_common::sched::MachineModel;

use arm64::Arm64Dialect;
use tir_riscv::RiscvDialect;

/// Resolve a `--march`/`--mcpu` pair to a target, or `None` if no backend accepts it.
pub fn select(march: &str, mcpu: Option<&str>) -> Option<Box<dyn TargetMachine>> {
    if let Some(config) = tir_riscv::TargetConfig::parse(march, mcpu) {
        return Some(Box::new(RiscvTarget { config }));
    }

    if let Some(config) = arm64::TargetConfig::parse(march, mcpu) {
        return Some(Box::new(Arm64Target { config }));
    }

    None
}

/// Names accepted by [`select`], one canonical spelling per target family, for
/// help text and error messages.
pub const SUPPORTED_TARGETS: &[&str] = &["riscv32", "riscv64", "arm64"];

/// The RISC-V target.
pub struct RiscvTarget {
    config: tir_riscv::TargetConfig,
}

impl TargetMachine for RiscvTarget {
    fn name(&self) -> &'static str {
        self.config.canonical_name()
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
pub struct Arm64Target {
    config: arm64::TargetConfig,
}

impl TargetMachine for Arm64Target {
    fn name(&self) -> &'static str {
        self.config.canonical_name()
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
