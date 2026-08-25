//! Verification of machine IR.
//!
//! Machine IR is not SSA — block-parameter destruction leaves a parameter
//! defined once per predecessor — so the generic op-tree verifier does not
//! describe it. What it does have is one register notation, and these are its
//! rules:
//!
//! - an instruction names only values that still exist;
//! - every register slot of an instruction holds a value or a register, never
//!   neither: slots are read off in port order;
//! - a register slot holding a value must hold one whose class views the same
//!   register file at the same bit offset: a narrower class of that view is an
//!   allocation constraint, a different view is a type error;
//! - a register assignment must place a value in a register of a class sharing
//!   its own view;
//! - once a function carries an assignment, it is total: every register-typed
//!   value some instruction names has an entry.

use std::collections::HashSet;

use tir::{Context, Error, OpHandle, OpId, ValueId};

use crate::backend::registers::{PINS_ATTR, slot_pin};
use crate::backend::registers::{RegAssignment, RegSlot, reg_slots, value_class};
use crate::backend::{ARG_PINS_ATTR, ASSIGNMENT_ATTR, SymbolOp};

/// Verify the machine IR under `root`.
pub fn verify_machine_ir(context: &Context, root: OpId) -> Result<(), Error> {
    let mut stack = vec![root];
    while let Some(op_id) = stack.pop() {
        if !context.has_operation(op_id) {
            continue;
        }
        let op = context.get_op(op_id);
        verify_reg_slots(context, &op)?;
        verify_views(context, &RegAssignment::of_op(&op, ARG_PINS_ATTR))?;
        verify_slot_pins(context, &op)?;
        if op.is::<SymbolOp>() {
            verify_assignment(context, &op)?;
        }
        for region in op.regions().iter().copied() {
            for block in context.get_region(region).iter(context.clone()) {
                stack.extend(block.op_ids());
            }
        }
    }
    Ok(())
}

fn verify_reg_slots(context: &Context, op: &OpHandle) -> Result<(), Error> {
    // An operand or result the rest of the IR has retired names nothing: the op
    // that produced it went, and this one was not rewritten with it.
    for value in op.operands().iter().chain(op.results().iter()) {
        if !context.has_value(*value) {
            return Err(Error::VerificationError(format!(
                "{} names %{}, which no longer exists",
                op.name().as_str(),
                value.number(),
            )));
        }
    }
    let slots = reg_slots(op);
    // A slot is an SSA position or an attribute, and every port has one:
    // positions are read off in port order, so a port with neither would be
    // read as the next port's.
    for port in crate::backend::reg_ports(op) {
        if op.attr(port.name).is_some() || slots.iter().any(|slot| slot.port.name == port.name) {
            continue;
        }
        return Err(Error::VerificationError(format!(
            "{} register slot '{}' holds neither a value nor a register",
            op.name().as_str(),
            port.name,
        )));
    }
    for slot in &slots {
        let (RegSlot::Value(value), Some(port_class)) = (slot.slot, slot.port.class) else {
            continue;
        };
        let Some(class) = value_class(context, value) else {
            continue;
        };
        // A register group read through a single-register slot — an RVV LMUL
        // group in a `VR` operand — is the same file at the same offset, and is
        // the allocation unit rather than a different view.
        if class.file() != port_class.file() || class.view.bit_offset != port_class.view.bit_offset
        {
            return Err(Error::VerificationError(format!(
                "{} operand '{}' reads %{} of class {} through {}, a different register view",
                op.name().as_str(),
                slot.port.name,
                value.number(),
                class.name(),
                port_class.name(),
            )));
        }
    }
    Ok(())
}

fn verify_assignment(context: &Context, symbol: &OpHandle) -> Result<(), Error> {
    if symbol.attr(ASSIGNMENT_ATTR).is_none() {
        return Ok(());
    }
    let assignment = RegAssignment::of_op(symbol, ASSIGNMENT_ATTR);
    verify_views(context, &assignment)?;
    for value in register_values(context, symbol) {
        if assignment.get(value).is_none() {
            return Err(Error::VerificationError(format!(
                "%{} has no register in the assignment of '{}'",
                value.number(),
                match symbol.attr("name") {
                    Some(tir::attributes::AttributeValue::Str(name)) => name.to_string(),
                    _ => String::new(),
                },
            )));
        }
    }
    Ok(())
}

/// A slot may only be pinned to a register of a class sharing the view of the
/// value it holds.
fn verify_slot_pins(context: &Context, op: &OpHandle) -> Result<(), Error> {
    if op.attr(PINS_ATTR).is_none() {
        return Ok(());
    }
    for slot in reg_slots(op) {
        let (RegSlot::Value(value), Some((class, index))) =
            (slot.slot, slot_pin(op, slot.port.name))
        else {
            continue;
        };
        let Some(value_class) = value_class(context, value) else {
            continue;
        };
        if class.file() != value_class.file()
            || class.view.bit_offset != value_class.view.bit_offset
        {
            return Err(Error::VerificationError(format!(
                "{} slot '{}' holds %{} of class {} but is pinned to {}[{}]",
                op.name().as_str(),
                slot.port.name,
                value.number(),
                value_class.name(),
                class.name(),
                index,
            )));
        }
    }
    Ok(())
}

/// A value may only be pinned to, or assigned, a register of a class sharing
/// its own architectural view.
fn verify_views(context: &Context, assignment: &RegAssignment) -> Result<(), Error> {
    for (value, (class, index)) in assignment.iter() {
        let Some(value_class) = value_class(context, value) else {
            continue;
        };
        if !class.shares_view_with(value_class) {
            return Err(Error::VerificationError(format!(
                "%{} of class {} is pinned to {}[{}], a different register view",
                value.number(),
                value_class.name(),
                class.name(),
                index,
            )));
        }
    }
    Ok(())
}

/// Every register-typed value the symbol's instructions name. A block parameter
/// no instruction names needs no register: its predecessors' copies were
/// rewritten away with it.
fn register_values(context: &Context, symbol: &OpHandle) -> Vec<ValueId> {
    let mut seen = HashSet::new();
    let mut values = Vec::new();
    let record = |value: ValueId, seen: &mut HashSet<ValueId>, values: &mut Vec<ValueId>| {
        if context.has_value(value) && value_class(context, value).is_some() && seen.insert(value) {
            values.push(value);
        }
    };
    for region in context.get_op(symbol.id).regions().iter().copied() {
        for block in context.get_region(region).iter(context.clone()) {
            for op_id in block.op_ids() {
                let op = context.get_op(op_id);
                for value in op.operands().iter().chain(op.results().iter()) {
                    record(*value, &mut seen, &mut values);
                }
            }
        }
    }
    values
}
