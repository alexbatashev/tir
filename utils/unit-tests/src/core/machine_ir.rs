//! The one register notation: register slots as SSA ports, physical literals,
//! the assignment map, and the rules the machine-IR verifier enforces over them.
//!
//! Machine instructions have no textual form, so these build the IR directly
//! rather than through a `.tir` check.

use tir::attributes::AttributeValue;
use tir::backend::regalloc::{RegClassId, RegClassInfo, RegisterView};
use tir::backend::{
    phys_attr, verify_machine_ir, ControlFlow, InstrInfo, MachineInstruction, RegAssignment,
    RegClassType, RegPort, SymbolOpBuilder, ASSIGNMENT_ATTR, PINS_ATTR,
};
use tir::{Context, Operation, ValueId};

use super::fixtures::r;

/// A second class over the `R` file at a different bit offset: no register
/// satisfies both views (an x86 high-byte class against an offset-0 one).
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

const fn r_high() -> RegClassId {
    RegClassId::new(&R_HIGH_CLASS)
}

// `add rd, rs`: one destination slot and one source slot, both of class `R`,
// plus the implicit flag register the behavior writes.
tir::helpers::operation! {
    AddTestOp {
        name: "add",
        dialect: "test",
        operands: O { rs: "?tir::backend::RegClassType", },
        results: R { regs: "*tir::backend::RegClassType" },
        interfaces: [tir::backend::MachineInstruction],
    }
}

static ADD_PORTS: [RegPort; 2] = [
    RegPort {
        name: "rd",
        class: Some(r()),
        def: true,
        tied_to: None,
    },
    RegPort {
        name: "rs",
        class: Some(r()),
        def: false,
        tied_to: None,
    },
];

static ADD_IMPLICIT: [tir::attributes::ImplicitReg; 1] = [tir::attributes::ImplicitReg {
    class: r(),
    index: 7,
    role: tir::attributes::AttributeRole::Def,
}];

impl MachineInstruction for AddTestOp {
    fn info(&self) -> &'static InstrInfo {
        static INFO: InstrInfo = InstrInfo {
            name: "add",
            mnemonic: "add",
            control_flow: ControlFlow::None,
            regs: &ADD_PORTS,
            implicit_regs: &ADD_IMPLICIT,
            ..InstrInfo::BASE
        };
        &INFO
    }

    fn instance(&self) -> &tir::OpHandle {
        &self.0
    }
}

fn value_of(context: &Context, class: RegClassId) -> ValueId {
    context
        .create_value(RegClassType::new(context, class), None)
        .id()
}

/// An `asm.symbol` holding one `add` in its body, built with whatever slots the
/// caller asks for.
struct Function {
    context: Context,
    symbol: tir::backend::SymbolOp,
    add: tir::OpHandle,
}

fn function(build: impl FnOnce(&Context, AddTestOpBuilder) -> AddTestOpBuilder) -> Function {
    let context = Context::with_default_dialects();
    AddTestOp::register_interfaces(&context);
    let add = build(&context, AddTestOpBuilder::new(&context)).build();
    let handle = add.get_handle();
    let block = context.create_block(vec![]);
    block.append(add.id());
    let region = context.create_region();
    region.add_block(block.id());
    let symbol = SymbolOpBuilder::new(&context)
        .body(region.id())
        .attr("name", AttributeValue::Str("f".into()))
        .build();
    Function {
        context,
        symbol,
        add: handle,
    }
}

impl AddTestOp {
    fn get_handle(&self) -> tir::OpHandle {
        self.0.clone()
    }
}

/// Def-use over machine IR reads the SSA ports; the physical registers an
/// instruction names directly, and the ones its behavior touches implicitly,
/// are not values and stay out of the value chains.
#[test]
fn def_use_reads_ports_and_leaves_physical_registers_out() {
    let f = function(|context, builder| {
        let source = value_of(context, r());
        builder
            .rs(source)
            .result_types(vec![RegClassType::new(context, r())])
    });
    let regs = tir::analysis::op_regs(&f.add);
    assert_eq!(regs.defs.len(), 1, "the destination slot is a result");
    assert_eq!(regs.uses.len(), 1, "the source slot is an operand");
    assert!(regs.phys_defs.is_empty());
    assert!(regs.phys_uses.is_empty());

    // The same instruction reading a register it names directly: no operand,
    // one physical read.
    let f = function(|context, builder| {
        builder
            .attr("rs", phys_attr((r(), 3)))
            .result_types(vec![RegClassType::new(context, r())])
    });
    let regs = tir::analysis::op_regs(&f.add);
    assert!(regs.uses.is_empty());
    assert_eq!(regs.phys_uses, vec![(r(), 3)]);

    // Execution additionally sees the register the behavior writes by path.
    let execution = tir::analysis::execution_regs(&f.add);
    assert_eq!(execution.phys_defs, vec![(r(), 7)]);
}

/// A slot may only read a value whose class views the same register file at the
/// same offset. A high-byte slot reading an offset-0 value is a type error, not
/// an allocation constraint.
#[test]
fn cross_view_operand_is_rejected() {
    let f = function(|context, builder| {
        let source = value_of(context, r_high());
        builder
            .rs(source)
            .result_types(vec![RegClassType::new(context, r())])
    });
    let error = verify_machine_ir(&f.context, f.symbol.id()).expect_err("cross-view operand");
    assert!(
        format!("{error:?}").contains("different register view"),
        "{error:?}",
    );
}

/// A register slot holds a register-typed value: a mid-end value that reached
/// an instruction unretyped has no class for allocation to place it by.
#[test]
fn operand_without_a_register_class_is_rejected() {
    let f = function(|context, builder| {
        let source = context
            .create_value(tir::builtin::IntegerType::new(context, 32), None)
            .id();
        builder
            .rs(source)
            .result_types(vec![RegClassType::new(context, r())])
    });
    let error = verify_machine_ir(&f.context, f.symbol.id()).expect_err("class-less operand");
    assert!(format!("{error:?}").contains("not a register"), "{error:?}");
}

/// Every SSA position is some port's: a second result on a one-def opcode
/// would be defined by nothing.
#[test]
fn a_result_without_a_port_is_rejected() {
    let f = function(|context, builder| {
        let source = value_of(context, r());
        builder.rs(source).result_types(vec![
            RegClassType::new(context, r()),
            RegClassType::new(context, r()),
        ])
    });
    let error = verify_machine_ir(&f.context, f.symbol.id()).expect_err("surplus result");
    assert!(format!("{error:?}").contains("register slots"), "{error:?}");
}

/// Once a function carries an assignment it is total: a value an instruction
/// names with no register is an unfinished allocation, not a free choice.
#[test]
fn assignment_missing_a_value_is_rejected() {
    let f = function(|context, builder| {
        let source = value_of(context, r());
        builder
            .rs(source)
            .result_types(vec![RegClassType::new(context, r())])
    });
    let mut assignment = RegAssignment::default();
    for value in f.add.results().iter().chain(f.add.operands().iter()) {
        assignment.insert(*value, (r(), 0));
    }
    let attrs = |context: &Context, map: &RegAssignment| {
        let mut attrs = f.symbol.attributes();
        attrs.retain(|attr| context.resolve(attr.name) != ASSIGNMENT_ATTR);
        attrs.push(context.named_attribute(ASSIGNMENT_ATTR, map.to_attribute()));
        attrs
    };
    f.context
        .set_op_attributes(f.symbol.id(), attrs(&f.context, &assignment));
    verify_machine_ir(&f.context, f.symbol.id()).expect("a total assignment verifies");

    // Drop the source's entry: the instruction still names it.
    let mut partial = RegAssignment::default();
    partial.insert(f.add.results()[0], (r(), 0));
    f.context
        .set_op_attributes(f.symbol.id(), attrs(&f.context, &partial));
    let error = verify_machine_ir(&f.context, f.symbol.id()).expect_err("partial assignment");
    assert!(
        format!("{error:?}").contains("no register in the assignment"),
        "{error:?}",
    );
}

/// A pin belongs to a slot, and may only name a register of the view the value
/// in that slot lives in.
#[test]
fn cross_view_pin_is_rejected() {
    let f = function(|context, builder| {
        let source = value_of(context, r());
        builder
            .rs(source)
            .result_types(vec![RegClassType::new(context, r())])
    });
    let mut pins = std::collections::BTreeMap::new();
    pins.insert("rs".to_string(), phys_attr((r_high(), 0)));
    let mut attrs = f.add.attributes();
    attrs.push(
        f.context
            .named_attribute(PINS_ATTR, AttributeValue::Dict(Box::new(pins))),
    );
    f.context.set_op_attributes(f.add.id, attrs);

    let error = verify_machine_ir(&f.context, f.symbol.id()).expect_err("cross-view pin");
    assert!(
        format!("{error:?}").contains("is pinned to Rhigh[0]"),
        "{error:?}"
    );
}

/// Slots are read off in port order, so a port holding neither a value nor a
/// register would be read as the next port's.
#[test]
fn a_slot_holding_nothing_is_rejected() {
    let f = function(|_, builder| builder);
    let error = verify_machine_ir(&f.context, f.symbol.id()).expect_err("empty slot");
    assert!(
        format!("{error:?}").contains("neither a value nor a register"),
        "{error:?}",
    );
}

/// The assignment is written into the IR and read back out of it: the map is
/// data, not a rewrite, so it has to survive a round trip through the text.
#[test]
fn the_assignment_round_trips_through_the_text_form() {
    let context = Context::with_default_dialects();
    let mut assignment = RegAssignment::default();
    assignment.insert(ValueId::from_number(3), (r(), 1));
    assignment.insert(ValueId::from_number(7), (r(), 2));

    let mut printed = String::new();
    let mut formatter = tir::IRFormatter::new(&mut printed);
    assignment
        .to_attribute()
        .print(&mut formatter, &context)
        .expect("prints");
    assert_eq!(printed, "[%3:R[1], %7:R[2]]");

    context.register_reg_classes(&super::fixtures::R_CLASSES);
    let mut parser = tir::parse::text::Parser::new(&printed);
    let parsed = parser
        .parse_attribute_value(&context)
        .expect("parses")
        .expect("an attribute");
    let symbol = SymbolOpBuilder::new(&context)
        .attr("name", AttributeValue::Str("f".into()))
        .attr(ASSIGNMENT_ATTR, parsed)
        .build();
    let read_back = RegAssignment::of_op(symbol.handle(), ASSIGNMENT_ATTR);
    assert_eq!(read_back, assignment);
}
