use std::collections::{BTreeSet, HashMap};
use std::io::Write;

use crate::Type;
use crate::ast;
use crate::error::TMDLError;
use crate::utils::resolve_operands_for_instruction;

pub fn generate_sail_properties<'a>(
    dialect: &str,
    files: &'a [ast::File],
    item_cache: &HashMap<&'a str, &'a ast::Item>,
    mut output: Box<dyn Write>,
) -> Result<(), TMDLError> {
    writeln!(
        output,
        "$ifndef TMDL_SAIL_PROPERTY_GENERATED\n$define TMDL_SAIL_PROPERTY_GENERATED\n\n/*\n * Auto-generated Sail property harness from TMDL.\n *\n * This file is ISA-agnostic: it only refers to instruction metadata and\n * behavior captured in TMDL. A concrete Sail model should provide the\n * hook functions declared below to map model state to the TMDL view.\n */"
    )?;

    writeln!(
        output,
        "\n/* ----- Integration hooks supplied by the target Sail model ----- */"
    )?;
    writeln!(output, "val tmdl_state_snapshot : unit -> bits(0)")?;
    writeln!(output, "val tmdl_decode_accepts : bits(32) -> bool")?;
    writeln!(
        output,
        "val tmdl_step_from_encoding : (bits(0), bits(32)) -> bits(0)"
    )?;
    writeln!(
        output,
        "val tmdl_step_from_name : (bits(0), string, bits(32)) -> bits(0)"
    )?;
    writeln!(output, "val tmdl_state_equiv : (bits(0), bits(0)) -> bool")?;
    writeln!(
        output,
        "val tmdl_reg_update_equiv : (bits(0), bits(0), bits(0), string, int) -> bool"
    )?;

    for inst in files.iter().flat_map(|f| f.instructions()) {
        emit_instruction_properties(dialect, inst, item_cache, &mut output)?;
    }

    writeln!(output, "\n$endif /* TMDL_SAIL_PROPERTY_GENERATED */")?;
    Ok(())
}

fn emit_instruction_properties<'a>(
    dialect: &str,
    instruction: &'a ast::Instruction,
    item_cache: &HashMap<&'a str, &'a ast::Item>,
    output: &mut Box<dyn Write>,
) -> Result<(), TMDLError> {
    let name = instruction.name.to_lowercase();
    let operands = resolve_operands_for_instruction(instruction, item_cache);
    let quant_sig = sail_quantified_operands(&operands);
    let encode_ty_sig = sail_encode_type_signature(&operands);
    let operand_call = sail_operand_call_list(&operands);
    let encoding_expr = format!("tmdl_encode_{dialect}_{name}{operand_call}");

    writeln!(output, "\n/* ---- {dialect}.{name} ---- */")?;
    writeln!(output, "val tmdl_encode_{dialect}_{name} : {encode_ty_sig}")?;

    writeln!(
        output,
        "$property tmdl_prop_{dialect}_{name}_encoding_valid\n  forall ({quant_sig}pre : bits(0)).\n    let enc = {encoding_expr};\n    tmdl_decode_accepts(enc)"
    )?;

    writeln!(
        output,
        "$property tmdl_prop_{dialect}_{name}_state_equiv\n  forall ({quant_sig}pre : bits(0)).\n    let enc = {encoding_expr};\n    let post_tmdl = tmdl_step_from_name((pre, \"{name}\", enc));\n    let post_sail = tmdl_step_from_encoding((pre, enc));\n    tmdl_state_equiv((post_tmdl, post_sail))"
    )?;

    let updated_regs = collect_updated_register_operands(instruction, &operands);
    for (op_name, ty) in updated_regs {
        let class_name = register_class_name(&ty);
        writeln!(
            output,
            "$property tmdl_prop_{dialect}_{name}_reg_{op_name}_equiv\n  forall ({quant_sig}pre : bits(0)).\n    let enc = {encoding_expr};\n    let post_tmdl = tmdl_step_from_name((pre, \"{name}\", enc));\n    let post_sail = tmdl_step_from_encoding((pre, enc));\n    tmdl_reg_update_equiv((pre, post_tmdl, post_sail, \"{class_name}\", int({op_name})))"
        )?;
    }

    Ok(())
}

fn sail_quantified_operands(operands: &[(String, Type)]) -> String {
    let mut parts = Vec::with_capacity(operands.len());
    for (name, ty) in operands {
        parts.push(format!(
            "{} : {}",
            name.to_lowercase(),
            sail_ty_of_operand(ty)
        ));
    }

    if parts.is_empty() {
        String::new()
    } else {
        format!("{}, ", parts.join(", "))
    }
}

fn sail_encode_type_signature(operands: &[(String, Type)]) -> String {
    if operands.is_empty() {
        "bits(32)".to_string()
    } else {
        let args = operands
            .iter()
            .map(|(_, ty)| sail_ty_of_operand(ty))
            .collect::<Vec<_>>()
            .join(", ");
        format!("({args}) -> bits(32)")
    }
}

fn sail_operand_call_list(operands: &[(String, Type)]) -> String {
    if operands.is_empty() {
        String::new()
    } else {
        let args = operands
            .iter()
            .map(|(name, _)| name.to_lowercase())
            .collect::<Vec<_>>()
            .join(", ");
        format!("({args})")
    }
}

fn sail_ty_of_operand(ty: &Type) -> &'static str {
    match ty {
        Type::Struct(_) => "int",
        Type::Bits(_) | Type::Integer => "bits(64)",
        Type::String => "string",
        _ => "int",
    }
}

fn register_class_name(ty: &Type) -> String {
    match ty {
        Type::Struct(name) => name.clone(),
        _ => "unknown".to_string(),
    }
}

fn collect_updated_register_operands(
    instruction: &ast::Instruction,
    operands: &[(String, Type)],
) -> Vec<(String, Type)> {
    let reg_operands = operands
        .iter()
        .filter_map(|(name, ty)| match ty {
            Type::Struct(_) => Some((name.clone(), ty.clone())),
            _ => None,
        })
        .collect::<HashMap<_, _>>();

    let mut written = BTreeSet::new();
    collect_assigned_idents(&instruction.behavior, &mut written);

    written
        .into_iter()
        .filter_map(|name| reg_operands.get(&name).cloned().map(|ty| (name, ty)))
        .collect()
}

fn collect_assigned_idents(expr: &ast::Expr, into: &mut BTreeSet<String>) {
    match expr {
        ast::Expr::Assign(assign) => {
            into.insert(assign.dest.clone());
        }
        ast::Expr::Block(block) => {
            for stmt in &block.stmts {
                collect_assigned_idents(stmt, into);
            }
        }
        ast::Expr::If(if_expr) => {
            collect_assigned_idents(&if_expr.then, into);
            if let Some(else_expr) = &if_expr.else_ {
                collect_assigned_idents(else_expr, into);
            }
        }
        _ => {}
    }
}
