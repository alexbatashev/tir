//! Backend liveness: class constraints, interference and fixpoint.

use std::collections::BTreeSet;

use tir::backend::liveness::analyze;
use tir::backend::regalloc::{RegClassId, RegClassInfo, RegisterView};
use tir::backend::{ControlFlow, InstrInfo, MachineInstruction, RegClassType, RegPort};
use tir::builtin::{ops, IntegerType};
use tir::{BlockHandle, Context, Operation, TypeId, ValueId};

use super::fixtures::r;

// The test ops: each names one register slot. A slot's class is a per-opcode
// fact, so there is one op per class the tests constrain a value through; the
// value's own class is its type.
macro_rules! slot_op {
    ($op:ident, $ports:ident, $name:literal, $class:expr, $def:literal) => {
        static $ports: [RegPort; 1] = [RegPort {
            name: "r",
            class: $class,
            def: $def,
            tied_to: None,
        }];

        impl MachineInstruction for $op {
            fn info(&self) -> &'static InstrInfo {
                static INFO: InstrInfo = InstrInfo {
                    name: $name,
                    mnemonic: $name,
                    control_flow: ControlFlow::None,
                    regs: &$ports,
                    ..InstrInfo::BASE
                };
                &INFO
            }

            fn instance(&self) -> &tir::OpHandle {
                &self.0
            }
        }
    };
}

tir::helpers::operation! {
    PhysDefOp {
        name: "phys_def",
        dialect: "test",
        operands: O { r: "?tir::backend::RegClassType", },
        results: R { regs: "*tir::backend::RegClassType" },
        interfaces: [tir::backend::MachineInstruction],
    }
}

tir::helpers::operation! {
    PhysUseOp {
        name: "phys_use",
        dialect: "test",
        operands: O { r: "?tir::backend::RegClassType", },
        results: R { regs: "*tir::backend::RegClassType" },
        interfaces: [tir::backend::MachineInstruction],
    }
}

tir::helpers::operation! {
    UseROp {
        name: "use_r",
        dialect: "test",
        operands: O { r: "?tir::backend::RegClassType", },
        results: R { regs: "*tir::backend::RegClassType" },
        interfaces: [tir::backend::MachineInstruction],
    }
}

tir::helpers::operation! {
    UseRlowOp {
        name: "use_rlow",
        dialect: "test",
        operands: O { r: "?tir::backend::RegClassType", },
        results: R { regs: "*tir::backend::RegClassType" },
        interfaces: [tir::backend::MachineInstruction],
    }
}

tir::helpers::operation! {
    UseRhighOp {
        name: "use_rhigh",
        dialect: "test",
        operands: O { r: "?tir::backend::RegClassType", },
        results: R { regs: "*tir::backend::RegClassType" },
        interfaces: [tir::backend::MachineInstruction],
    }
}

tir::helpers::operation! {
    UseRmidOp {
        name: "use_rmid",
        dialect: "test",
        operands: O { r: "?tir::backend::RegClassType", },
        results: R { regs: "*tir::backend::RegClassType" },
        interfaces: [tir::backend::MachineInstruction],
    }
}

tir::helpers::operation! {
    UseROtherOp {
        name: "use_rother",
        dialect: "test",
        operands: O { r: "?tir::backend::RegClassType", },
        results: R { regs: "*tir::backend::RegClassType" },
        interfaces: [tir::backend::MachineInstruction],
    }
}

slot_op!(PhysDefOp, PHYS_DEF_PORTS, "phys_def", None, true);
slot_op!(PhysUseOp, PHYS_USE_PORTS, "phys_use", None, false);
slot_op!(UseROp, USE_R_PORTS, "use_r", Some(r()), false);
slot_op!(UseRlowOp, USE_RLOW_PORTS, "use_rlow", Some(r_low()), false);
slot_op!(
    UseRhighOp,
    USE_RHIGH_PORTS,
    "use_rhigh",
    Some(r_high()),
    false
);
slot_op!(UseRmidOp, USE_RMID_PORTS, "use_rmid", Some(r_mid()), false);
slot_op!(
    UseROtherOp,
    USE_ROTHER_PORTS,
    "use_rother",
    Some(r_other()),
    false
);

// A subclass of `R` over the same file and view: fewer encodable registers.
static R_LOW_CLASS: RegClassInfo = RegClassInfo {
    name: "Rlow",
    dialect: "test",
    file: "R",
    registers: &[0, 1],
    group_width: 1,
    view: RegisterView {
        bit_offset: 0,
        merge: false,
    },
    print_name: tir::backend::regalloc::no_register_name,
};

// Same file and index set as `Rlow`, but a different architectural view (an
// x86 high-byte class): no register satisfies both constraints.
static R_HIGH_CLASS: RegClassInfo = RegClassInfo {
    name: "Rhigh",
    dialect: "test",
    file: "R",
    registers: &[0, 1],
    group_width: 1,
    view: RegisterView {
        bit_offset: 8,
        merge: true,
    },
    print_name: tir::backend::regalloc::no_register_name,
};

const fn r_low() -> RegClassId {
    RegClassId::new(&R_LOW_CLASS)
}

const fn r_high() -> RegClassId {
    RegClassId::new(&R_HIGH_CLASS)
}

// Two classes over one view where neither contains the other (x86 `GPR32low`,
// which includes esp, and `GPRaddrIndex`, which excludes rsp but reaches r8+).
static R_MID_CLASS: RegClassInfo = RegClassInfo {
    name: "Rmid",
    dialect: "test",
    file: "R",
    registers: &[1, 2, 3],
    group_width: 1,
    view: RegisterView {
        bit_offset: 0,
        merge: false,
    },
    print_name: tir::backend::regalloc::no_register_name,
};

const fn r_mid() -> RegClassId {
    RegClassId::new(&R_MID_CLASS)
}

// Over one view with `Rlow`, but sharing no register with it.
static R_OTHER_CLASS: RegClassInfo = RegClassInfo {
    name: "Rother",
    dialect: "test",
    file: "R",
    registers: &[2, 3],
    group_width: 1,
    view: RegisterView {
        bit_offset: 0,
        merge: false,
    },
    print_name: tir::backend::regalloc::no_register_name,
};

const fn r_other() -> RegClassId {
    RegClassId::new(&R_OTHER_CLASS)
}

// A value of class `class`, the only thing its type says about it.
fn reg_value(context: &Context, class: RegClassId) -> ValueId {
    context
        .create_value(RegClassType::new(context, class), None)
        .id()
}

// Append an op reading `value` through the register slot of class `class`.
fn vreg_use(context: &Context, block: &BlockHandle, value: ValueId, class: RegClassId) {
    macro_rules! read {
        ($op:ident, $builder:ident) => {{
            $op::register_interfaces(context);
            $builder::new(context).r(value).build().id()
        }};
    }
    let op = match class.name() {
        "R" => read!(UseROp, UseROpBuilder),
        "Rlow" => read!(UseRlowOp, UseRlowOpBuilder),
        "Rhigh" => read!(UseRhighOp, UseRhighOpBuilder),
        "Rmid" => read!(UseRmidOp, UseRmidOpBuilder),
        "Rother" => read!(UseROtherOp, UseROtherOpBuilder),
        other => unreachable!("no test op reads through {other}"),
    };
    block.append(op);
}

// A value constrained by two classes over one file and view must end up in the
// narrower one — the wider constraint is satisfied by every register of the
// narrower, but not the other way round. Order of appearance is irrelevant.
#[test]
fn narrower_class_constraint_wins() {
    for wide_first in [true, false] {
        let context = Context::with_default_dialects();
        let a = reg_value(&context, r());
        let block = context.create_block(vec![context.get_value(a)]);

        if wide_first {
            vreg_use(&context, &block, a, r());
            vreg_use(&context, &block, a, r_low());
        } else {
            vreg_use(&context, &block, a, r_low());
            vreg_use(&context, &block, a, r());
        }

        let liveness = analyze(&context, &[block.id()], |_| Vec::new());
        assert_eq!(
            liveness.vreg_class.get(&a.number()),
            Some(&r_low()),
            "the narrower operand class must survive (wide first: {wide_first})",
        );
        assert_eq!(
            liveness.allowed_indices.get(&a.number()),
            Some(&BTreeSet::from([0, 1])),
        );
        assert!(liveness.class_conflicts.is_empty());
    }
}

// Two classes over one view where neither contains the other: the value is
// allocatable from the indices both encode, and nothing else.
#[test]
fn overlapping_classes_intersect_their_indices() {
    let context = Context::with_default_dialects();
    let a = reg_value(&context, r());
    let block = context.create_block(vec![context.get_value(a)]);

    vreg_use(&context, &block, a, r_low()); // {0, 1}
    vreg_use(&context, &block, a, r_mid()); // {1, 2, 3}

    let liveness = analyze(&context, &[block.id()], |_| Vec::new());
    assert!(liveness.class_conflicts.is_empty());
    assert_eq!(
        liveness.allowed_indices.get(&a.number()),
        Some(&BTreeSet::from([1])),
    );
}

// Classes over one view with no register in common cannot both be honored.
#[test]
fn disjoint_classes_over_one_view_are_reported() {
    let context = Context::with_default_dialects();
    let a = reg_value(&context, r());
    let block = context.create_block(vec![context.get_value(a)]);

    vreg_use(&context, &block, a, r_low()); // {0, 1}
    vreg_use(&context, &block, a, r_other()); // {2, 3}

    let liveness = analyze(&context, &[block.id()], |_| Vec::new());
    assert!(liveness.class_conflicts.contains_key(&a.number()));
}

// A value's own class and the class of a slot reading it must share a view;
// here they do not (an x86 high-byte slot reading an offset-0 value), and the
// allocator must be told rather than silently keeping one.
#[test]
fn incompatible_class_constraints_are_reported() {
    let context = Context::with_default_dialects();
    let a = reg_value(&context, r_low());
    let block = context.create_block(vec![context.get_value(a)]);

    vreg_use(&context, &block, a, r_high());

    let liveness = analyze(&context, &[block.id()], |_| Vec::new());
    assert_eq!(
        liveness.class_conflicts.get(&a.number()),
        Some(&(r_low(), r_high())),
    );
}

// `addi %a, %b` whose fresh result names a new virtual register (a def), with
// its two operands read as uses — enough for liveness, which resolves builtin
// SSA ops positionally.
fn addi(context: &Context, block: &BlockHandle, a: ValueId, b: ValueId, ty: TypeId) -> ValueId {
    block
        .append_op(ops::addi(context, a, b, ty).build())
        .result()
}

// Two defs in the entry block where the first is used only in a successor
// block: the two entry defs interfere iff the successor edge is wired, because
// that is what keeps the first value live across the second's def. With the
// edge dropped (the old `|_| Vec::new()`), the first value looks dead at its
// def and the allocator is free to reuse its register — the miscompile.
#[test]
fn cross_block_def_interferes_only_with_wired_successors() {
    let context = Context::with_default_dialects();
    let ty = IntegerType::new(&context, 64);
    let a = context.create_value(ty, None);
    let a_id = a.id();
    let entry = context.create_block(vec![a]);
    let succ = context.create_block(vec![]);

    // `v` is used only in the successor (so it is live across the edge); `w`
    // is defined after `v` and dies inside the entry block (consumed by `u`).
    // Their interference therefore hinges entirely on `v` being live-out.
    let v = addi(&context, &entry, a_id, a_id, ty);
    let w = addi(&context, &entry, a_id, a_id, ty);
    addi(&context, &entry, w, w, ty);
    addi(&context, &succ, v, a_id, ty);

    let blocks = [entry.id(), succ.id()];
    let with_edge = analyze(&context, &blocks, |blk| {
        if blk == entry.id() {
            vec![succ.id()]
        } else {
            vec![]
        }
    });
    assert!(
        with_edge.interferes(v.number(), w.number()),
        "a value live across a later def must interfere with it",
    );
    assert!(
        with_edge.live_in[&succ.id()].contains(&v.number()),
        "the cross-block value is live into its using block",
    );

    let no_edge = analyze(&context, &blocks, |_| Vec::new());
    assert!(
        !no_edge.interferes(v.number(), w.number()),
        "without the CFG edge the bug hides the interference (regression guard)",
    );
}

// Diamond: entry defines a value used only at the merge, so it is live-through
// both arms and must interfere with every def on either arm.
#[test]
fn diamond_live_through_interferes_on_both_arms() {
    let context = Context::with_default_dialects();
    let ty = IntegerType::new(&context, 64);
    let a = context.create_value(ty, None);
    let a_id = a.id();
    let entry = context.create_block(vec![a]);
    let left = context.create_block(vec![]);
    let right = context.create_block(vec![]);
    let merge = context.create_block(vec![]);

    let v = addi(&context, &entry, a_id, a_id, ty);
    let la = addi(&context, &left, a_id, a_id, ty);
    let ra = addi(&context, &right, a_id, a_id, ty);
    addi(&context, &merge, v, a_id, ty);

    let blocks = [entry.id(), left.id(), right.id(), merge.id()];
    let liveness = analyze(&context, &blocks, |blk| {
        if blk == entry.id() {
            vec![left.id(), right.id()]
        } else if blk == left.id() || blk == right.id() {
            vec![merge.id()]
        } else {
            vec![]
        }
    });

    assert!(liveness.live_in[&left.id()].contains(&v.number()));
    assert!(liveness.live_in[&right.id()].contains(&v.number()));
    assert!(
        liveness.interferes(v.number(), la.number()),
        "live-through value must interfere with the left arm's def",
    );
    assert!(
        liveness.interferes(v.number(), ra.number()),
        "live-through value must interfere with the right arm's def",
    );
}

// Append an op that reads (`is_def == false`) or writes (`is_def == true`) the
// physical register `class[index]` via a role-tagged register attribute.
fn phys_op(context: &Context, block: &BlockHandle, class: RegClassId, index: u16, is_def: bool) {
    use tir::attributes::{AttributeValue, RegisterAttr};

    // The test dialect is never registered, so hook the role interfaces in
    // directly.
    PhysDefOp::register_interfaces(context);
    PhysUseOp::register_interfaces(context);
    let register = AttributeValue::Register(RegisterAttr::Physical { class, index });
    let id = if is_def {
        PhysDefOpBuilder::new(context)
            .attr("r", register)
            .build()
            .id()
    } else {
        PhysUseOpBuilder::new(context)
            .attr("r", register)
            .build()
            .id()
    };
    block.append(id);
}

// A fixed-register read protocol: `def P; def v1; use P; use v1`. `v1` is live
// across the read of the physical register `P`, so it must not be colored `P` —
// otherwise the allocator could park it in `P` between `P`'s def and this read.
#[test]
fn physical_read_forbids_live_vreg() {
    let context = Context::with_default_dialects();
    let ty = IntegerType::new(&context, 64);
    let a = context.create_value(ty, None);
    let a_id = a.id();
    let block = context.create_block(vec![a]);

    phys_op(&context, &block, r(), 0, true); // def P
    let v1 = addi(&context, &block, a_id, a_id, ty); // def v1 (live across the read)
    phys_op(&context, &block, r(), 0, false); // use P
    addi(&context, &block, v1, a_id, ty); // use v1

    let liveness = analyze(&context, &[block.id()], |_| Vec::new());

    assert!(
        liveness.forbidden[&v1.number()].contains(&(r(), 0)),
        "a vreg live across a physical-register read must be forbidden from it",
    );
}

// A back edge (a loop): the fixpoint must converge, and a value defined in the
// header and read inside the body stays live around the edge.
#[test]
fn loop_back_edge_converges() {
    let context = Context::with_default_dialects();
    let ty = IntegerType::new(&context, 64);
    let a = context.create_value(ty, None);
    let a_id = a.id();
    let header = context.create_block(vec![a]);
    let body = context.create_block(vec![]);

    let carried = addi(&context, &header, a_id, a_id, ty);
    addi(&context, &body, carried, a_id, ty);

    // header -> body -> header (back edge).
    let blocks = [header.id(), body.id()];
    let liveness = analyze(&context, &blocks, |blk| {
        if blk == header.id() {
            vec![body.id()]
        } else {
            vec![header.id()]
        }
    });

    assert!(
        liveness.live_in[&body.id()].contains(&carried.number()),
        "the header-defined value is live into the loop body",
    );
}
