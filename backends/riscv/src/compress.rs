//! Post-RA compression: rewrite base instructions into their 16-bit C forms
//! once registers and immediates are physical. Running after allocation keeps
//! the C extension's operand restrictions (tied destinations, the x8..x15
//! subset, small scaled immediates) out of the allocator's search space:
//! selection and allocation target the full ISA, and every instruction that
//! happens to satisfy a compressed form is narrowed here for free.
//!
//! PC-relative control flow (`jal`, conditional branches) is deliberately not
//! compressed: their targets are fixups patched at object emission, and the
//! binary writer has no branch relaxation, so a ±256B/±2KB compressed range
//! would turn a long branch into a hard error instead of a wider encoding.

use tir::Operation;
use tir::attributes::AttributeValue;
use tir::backend::{RegSlot, reg_slot};

use crate::{
    AddImmOp, AddImmWordOp, AddOp, AddWordOp, AndImmOp, AndOp, CAddImm4SpNOpBuilder,
    CAddImm16SpOpBuilder, CAddImmOpBuilder, CAddImmWordOpBuilder, CAddOpBuilder, CAddWordOpBuilder,
    CAndImmOpBuilder, CAndOpBuilder, CEnvBreakOpBuilder, CFLoadDoubleOpBuilder,
    CFLoadDoubleSpOpBuilder, CFLoadWordOpBuilder, CFLoadWordSpOpBuilder, CFStoreDoubleOpBuilder,
    CFStoreDoubleSpOpBuilder, CFStoreWordOpBuilder, CFStoreWordSpOpBuilder,
    CJumpAndLinkRegOpBuilder, CJumpRegOpBuilder, CLoadDoubleOpBuilder, CLoadDoubleSpOpBuilder,
    CLoadImmOpBuilder, CLoadUpperImmOpBuilder, CLoadWordOpBuilder, CLoadWordSpOpBuilder,
    CMoveOpBuilder, CNopOpBuilder, COrOpBuilder, CShiftLeftLogicalImmOpBuilder,
    CShiftRightArithmeticImmOpBuilder, CShiftRightLogicalImmOpBuilder, CStoreDoubleOpBuilder,
    CStoreDoubleSpOpBuilder, CStoreWordOpBuilder, CStoreWordSpOpBuilder, CSubOpBuilder,
    CSubWordOpBuilder, CXorOpBuilder, EnvBreakOp, FLoadDoubleOp, FLoadWordOp, FStoreDoubleOp,
    FStoreWordOp, JumpAndLinkRegOp, LoadDoubleWordOp, LoadUpperImmOp, LoadWordOp, OrOp,
    ShiftLeftLogicalImmOp, ShiftRightArithmeticImmOp, ShiftRightLogicalImmOp, StoreDoubleWordOp,
    StoreWordOp, SubOp, SubWordOp, XorOp, phys,
};
use tir::backend::VirtualReturnOp;

pub(crate) fn compress_rv32(
    context: &tir::Context,
    op: &tir::OperationRef,
    rewriter: &mut tir::Rewriter,
) -> Result<bool, tir::PassError> {
    compress(context, op, rewriter, 32)
}

pub(crate) fn compress_rv64(
    context: &tir::Context,
    op: &tir::OperationRef,
    rewriter: &mut tir::Rewriter,
) -> Result<bool, tir::PassError> {
    compress(context, op, rewriter, 64)
}

/// A register slot of an instruction, carried through to the compressed form
/// unchanged: the same value stays in the same register, so the function's
/// assignment still describes it.
fn slot(op: &dyn Operation, name: &str) -> Option<RegSlot> {
    reg_slot(op.handle(), name)
}

/// An integer immediate operand. Symbol/block operands (fixups) return None,
/// keeping their instruction uncompressed.
fn imm(op: &dyn Operation, name: &str) -> Option<i64> {
    match op.attr(name)? {
        AttributeValue::Int(value) => Some(value),
        _ => None,
    }
}

/// x8..x15 (f8..f15): the registers a 3-bit field reaches.
fn is_c_reg(index: u16) -> bool {
    (8..=15).contains(&index)
}

/// A load/store offset that fits a zero-extended immediate of `bits` bits
/// scaled by `scale`.
fn fits_uimm(value: i64, bits: u32, scale: i64) -> bool {
    value >= 0 && value % scale == 0 && value < (1 << bits)
}

fn fits_simm6(value: i64) -> bool {
    (-32..32).contains(&value)
}

fn compress(
    context: &tir::Context,
    op: &tir::OperationRef,
    rewriter: &mut tir::Rewriter,
    xlen: u32,
) -> Result<bool, tir::PassError> {
    let replace = |rewriter: &mut tir::Rewriter, new_op: Box<dyn Operation>| {
        rewriter.replace_op(op, new_op.as_ref()).map(|()| true)
    };
    // Compression runs after allocation, so every register slot resolves to the
    // register it was given.
    let reg = |inner: &dyn Operation, name: &str| {
        tir::backend::op_slot_register(context, inner.handle(), name).map(|(_, index)| index)
    };

    // The return sequence compresses to `c.jr ra` (finalize would otherwise
    // expand it to the full `jalr x0, x1, 0`).
    if op.as_op::<VirtualReturnOp>().is_some() {
        let jr = CJumpRegOpBuilder::new(context)
            .attr("rs1", phys(&(crate::RegClass::GPR.id(), 1)))
            .build();
        return replace(rewriter, Box::new(jr));
    }

    if let Some(inner) = op.as_op::<AddImmOp>() {
        let (Some(rd), Some(rs1), Some(value)) =
            (reg(&inner, "rd"), reg(&inner, "rs1"), imm(&inner, "imm"))
        else {
            return Ok(false);
        };
        let rd_slot = slot(&inner, "rd").expect("checked above");
        if value == 0 {
            if rd == 0 && rs1 == 0 {
                return replace(rewriter, Box::new(CNopOpBuilder::new(context).build()));
            }
            if rd != 0 && rs1 != 0 {
                let mv = CMoveOpBuilder::new(context);
                let mv = tir::reg_use!(mv, rs2, slot(&inner, "rs1").expect("checked above"));
                let mv = tir::reg_def!(mv, rd, rd_slot);
                return replace(rewriter, Box::new(mv.build()));
            }
            return Ok(false);
        }
        if rd == 2 && rs1 == 2 && value % 16 == 0 && (-512..512).contains(&value) {
            // `c.addi16sp` names the stack pointer implicitly, in both
            // directions; the slots say which register that is.
            let sp = phys(&(crate::RegClass::GPR.id(), 2));
            let addi16sp = CAddImm16SpOpBuilder::new(context)
                .attr("x2", sp.clone())
                .attr("x2_def", sp)
                .attr("imm", AttributeValue::Int(value))
                .build();
            return replace(rewriter, Box::new(addi16sp));
        }
        let imm = AttributeValue::Int(value);
        if rd == rs1 && rd != 0 && fits_simm6(value) {
            let addi = CAddImmOpBuilder::new(context).attr("imm", imm);
            // `c.addi` is two-address: it reads the destination it writes.
            let addi = tir::reg_use!(addi, rd_tied, slot(&inner, "rs1").expect("checked above"));
            return replace(rewriter, Box::new(tir::reg_def!(addi, rd, rd_slot).build()));
        }
        if rs1 == 0 && rd != 0 && fits_simm6(value) {
            let li = CLoadImmOpBuilder::new(context).attr("imm", imm);
            return replace(rewriter, Box::new(tir::reg_def!(li, rd, rd_slot).build()));
        }
        if rs1 == 2 && is_c_reg(rd) && value > 0 && fits_uimm(value, 10, 4) {
            let addi4spn = CAddImm4SpNOpBuilder::new(context).attr("imm", imm);
            return replace(
                rewriter,
                Box::new(tir::reg_def!(addi4spn, rd, rd_slot).build()),
            );
        }
        return Ok(false);
    }

    if xlen == 64
        && let Some(inner) = op.as_op::<AddImmWordOp>()
    {
        let (Some(rd), Some(rs1), Some(value)) =
            (reg(&inner, "rd"), reg(&inner, "rs1"), imm(&inner, "imm"))
        else {
            return Ok(false);
        };
        if rd == rs1 && rd != 0 && fits_simm6(value) {
            let addiw = CAddImmWordOpBuilder::new(context).attr("imm", AttributeValue::Int(value));
            let addiw = tir::reg_use!(addiw, rd_tied, slot(&inner, "rs1").expect("checked above"));
            let addiw = tir::reg_def!(addiw, rd, slot(&inner, "rd").expect("checked above"));
            return replace(rewriter, Box::new(addiw.build()));
        }
        return Ok(false);
    }

    if let Some(inner) = op.as_op::<LoadUpperImmOp>() {
        let (Some(rd), Some(value)) = (reg(&inner, "rd"), imm(&inner, "imm")) else {
            return Ok(false);
        };
        // The 20-bit operand may carry the value in unsigned form; the
        // compressed form holds its 6 low bits sign-extended.
        let value = ((value & 0xFFFFF) << 44) >> 44;
        if rd != 0 && rd != 2 && value != 0 && fits_simm6(value) {
            let lui = CLoadUpperImmOpBuilder::new(context).attr("imm", AttributeValue::Int(value));
            let lui = tir::reg_def!(lui, rd, slot(&inner, "rd").expect("checked above"));
            return replace(rewriter, Box::new(lui.build()));
        }
        return Ok(false);
    }

    if let Some(inner) = op.as_op::<AddOp>() {
        let (Some(rd), Some(rs1), Some(rs2)) =
            (reg(&inner, "rd"), reg(&inner, "rs1"), reg(&inner, "rs2"))
        else {
            return Ok(false);
        };
        let rd_slot = slot(&inner, "rd").expect("checked above");
        let src = if rd == rs1 && rs2 != 0 {
            Some("rs2")
        } else if rd == rs2 && rs1 != 0 {
            Some("rs1")
        } else {
            None
        };
        if rd != 0
            && let Some(src) = src
        {
            let add = CAddOpBuilder::new(context);
            // `c.add` is two-address: the destination is also the first source.
            let tied = if src == "rs2" { "rs1" } else { "rs2" };
            let add = tir::reg_use!(add, rd_tied, slot(&inner, tied).expect("checked above"));
            let add = tir::reg_use!(add, rs2, slot(&inner, src).expect("checked above"));
            return replace(rewriter, Box::new(tir::reg_def!(add, rd, rd_slot).build()));
        }
        let src = if rs1 == 0 && rs2 != 0 {
            Some("rs2")
        } else if rs2 == 0 && rs1 != 0 {
            Some("rs1")
        } else {
            None
        };
        if rd != 0
            && let Some(src) = src
        {
            let mv = CMoveOpBuilder::new(context);
            let mv = tir::reg_use!(mv, rs2, slot(&inner, src).expect("checked above"));
            return replace(rewriter, Box::new(tir::reg_def!(mv, rd, rd_slot).build()));
        }
        return Ok(false);
    }

    // The CA-format two-address ALU ops over x8..x15. `sub`/`subw` are the
    // only non-commutative members.
    macro_rules! ca_op {
        ($ty:ty, $builder:ident, $commutative:expr) => {
            if let Some(inner) = op.as_op::<$ty>() {
                let (Some(rd), Some(rs1), Some(rs2)) =
                    (reg(&inner, "rd"), reg(&inner, "rs1"), reg(&inner, "rs2"))
                else {
                    return Ok(false);
                };
                let src = if rd == rs1 {
                    Some("rs2")
                } else if $commutative && rd == rs2 {
                    Some("rs1")
                } else {
                    None
                };
                if is_c_reg(rd)
                    && is_c_reg(rs1)
                    && is_c_reg(rs2)
                    && let Some(src) = src
                {
                    let tied = if src == "rs2" { "rs1" } else { "rs2" };
                    let new_op = $builder::new(context);
                    let new_op =
                        tir::reg_use!(new_op, rd_tied, slot(&inner, tied).expect("checked above"));
                    let new_op =
                        tir::reg_use!(new_op, rs2, slot(&inner, src).expect("checked above"));
                    let new_op =
                        tir::reg_def!(new_op, rd, slot(&inner, "rd").expect("checked above"));
                    return replace(rewriter, Box::new(new_op.build()));
                }
                return Ok(false);
            }
        };
    }
    ca_op!(SubOp, CSubOpBuilder, false);
    ca_op!(XorOp, CXorOpBuilder, true);
    ca_op!(OrOp, COrOpBuilder, true);
    ca_op!(AndOp, CAndOpBuilder, true);
    if xlen == 64 {
        ca_op!(SubWordOp, CSubWordOpBuilder, false);
        ca_op!(AddWordOp, CAddWordOpBuilder, true);
    }

    // Shift/and immediates.
    macro_rules! imm_alu {
        ($ty:ty, $builder:ident, $rd_ok:expr, $imm_ok:expr) => {
            if let Some(inner) = op.as_op::<$ty>() {
                let (Some(rd), Some(rs1), Some(value)) =
                    (reg(&inner, "rd"), reg(&inner, "rs1"), imm(&inner, "imm"))
                else {
                    return Ok(false);
                };
                #[allow(clippy::redundant_closure_call)]
                if rd == rs1 && ($rd_ok)(rd) && ($imm_ok)(value) {
                    let new_op = $builder::new(context).attr("imm", AttributeValue::Int(value));
                    let new_op =
                        tir::reg_use!(new_op, rd_tied, slot(&inner, "rs1").expect("checked above"));
                    let new_op =
                        tir::reg_def!(new_op, rd, slot(&inner, "rd").expect("checked above"));
                    return replace(rewriter, Box::new(new_op.build()));
                }
                return Ok(false);
            }
        };
    }
    let shamt_ok = |v: i64| v > 0 && v < xlen as i64;
    imm_alu!(
        ShiftLeftLogicalImmOp,
        CShiftLeftLogicalImmOpBuilder,
        |rd| rd != 0,
        shamt_ok
    );
    imm_alu!(
        ShiftRightLogicalImmOp,
        CShiftRightLogicalImmOpBuilder,
        is_c_reg,
        shamt_ok
    );
    imm_alu!(
        ShiftRightArithmeticImmOp,
        CShiftRightArithmeticImmOpBuilder,
        is_c_reg,
        shamt_ok
    );
    imm_alu!(AndImmOp, CAndImmOpBuilder, is_c_reg, fits_simm6);

    // Loads and stores: sp-relative forms take the full register set and a
    // wider offset; the general forms need both registers in x8..x15.
    macro_rules! mem_op {
        ($ty:ty, $dir:ident, $data:ident, $scale:literal, $sp_bits:literal, $c_bits:literal,
         $sp_builder:ident, $c_builder:ident, $data_ok:expr, $sp_data_ok:expr) => {
            if let Some(inner) = op.as_op::<$ty>() {
                let (Some(data), Some(rs1), Some(value)) = (
                    reg(&inner, stringify!($data)),
                    reg(&inner, "rs1"),
                    imm(&inner, "imm"),
                ) else {
                    return Ok(false);
                };
                let data_slot = slot(&inner, stringify!($data)).expect("checked above");
                #[allow(clippy::redundant_closure_call)]
                if rs1 == 2 && ($sp_data_ok)(data) && fits_uimm(value, $sp_bits, $scale) {
                    // The sp-relative forms name the stack pointer implicitly;
                    // the slot says which register that is.
                    let new_op = $sp_builder::new(context)
                        .attr("x2", phys(&(crate::RegClass::GPR.id(), 2)))
                        .attr("imm", AttributeValue::Int(value));
                    let new_op = tir::$dir!(new_op, $data, data_slot);
                    return replace(rewriter, Box::new(new_op.build()));
                }
                #[allow(clippy::redundant_closure_call)]
                if ($data_ok)(data) && is_c_reg(rs1) && fits_uimm(value, $c_bits, $scale) {
                    let new_op = $c_builder::new(context).attr("imm", AttributeValue::Int(value));
                    let new_op =
                        tir::reg_use!(new_op, rs1, slot(&inner, "rs1").expect("checked above"));
                    let new_op = tir::$dir!(new_op, $data, data_slot);
                    return replace(rewriter, Box::new(new_op.build()));
                }
                return Ok(false);
            }
        };
    }
    let any_reg = |_: u16| true;
    let not_zero = |r: u16| r != 0;
    mem_op!(
        LoadWordOp,
        reg_def,
        rd,
        4,
        8,
        7,
        CLoadWordSpOpBuilder,
        CLoadWordOpBuilder,
        is_c_reg,
        not_zero
    );
    mem_op!(
        StoreWordOp,
        reg_use,
        rs2,
        4,
        8,
        7,
        CStoreWordSpOpBuilder,
        CStoreWordOpBuilder,
        is_c_reg,
        any_reg
    );
    if xlen == 64 {
        mem_op!(
            LoadDoubleWordOp,
            reg_def,
            rd,
            8,
            9,
            8,
            CLoadDoubleSpOpBuilder,
            CLoadDoubleOpBuilder,
            is_c_reg,
            not_zero
        );
        mem_op!(
            StoreDoubleWordOp,
            reg_use,
            rs2,
            8,
            9,
            8,
            CStoreDoubleSpOpBuilder,
            CStoreDoubleOpBuilder,
            is_c_reg,
            any_reg
        );
    }
    // Float loads/stores: an fld/fsw op in the stream implies its base
    // extension (D/F) is enabled, so C's presence completes the Zcd/Zcf
    // conjunction. The word forms are rv32-only.
    mem_op!(
        FLoadDoubleOp,
        reg_def,
        fd,
        8,
        9,
        8,
        CFLoadDoubleSpOpBuilder,
        CFLoadDoubleOpBuilder,
        is_c_reg,
        any_reg
    );
    mem_op!(
        FStoreDoubleOp,
        reg_use,
        fs2,
        8,
        9,
        8,
        CFStoreDoubleSpOpBuilder,
        CFStoreDoubleOpBuilder,
        is_c_reg,
        any_reg
    );
    if xlen == 32 {
        mem_op!(
            FLoadWordOp,
            reg_def,
            fd,
            4,
            8,
            7,
            CFLoadWordSpOpBuilder,
            CFLoadWordOpBuilder,
            is_c_reg,
            any_reg
        );
        mem_op!(
            FStoreWordOp,
            reg_use,
            fs2,
            4,
            8,
            7,
            CFStoreWordSpOpBuilder,
            CFStoreWordOpBuilder,
            is_c_reg,
            any_reg
        );
    }

    if let Some(inner) = op.as_op::<JumpAndLinkRegOp>() {
        let (Some(rd), Some(rs1), Some(value)) =
            (reg(&inner, "rd"), reg(&inner, "rs1"), imm(&inner, "imm"))
        else {
            return Ok(false);
        };
        if value == 0 && rs1 != 0 {
            let rs1_slot = slot(&inner, "rs1").expect("checked above");
            if rd == 0 {
                let jr = CJumpRegOpBuilder::new(context);
                return replace(rewriter, Box::new(tir::reg_use!(jr, rs1, rs1_slot).build()));
            }
            if rd == 1 {
                let jalr = CJumpAndLinkRegOpBuilder::new(context);
                return replace(
                    rewriter,
                    Box::new(tir::reg_use!(jalr, rs1, rs1_slot).build()),
                );
            }
        }
        return Ok(false);
    }

    if op.as_op::<EnvBreakOp>().is_some() {
        return replace(rewriter, Box::new(CEnvBreakOpBuilder::new(context).build()));
    }

    Ok(false)
}
