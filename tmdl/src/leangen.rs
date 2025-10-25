use std::collections::HashMap;
use std::io::Write;

use crate::ast::{self, Item};
use crate::error::TMDLError;

pub fn generate_lean(
    files: Vec<ast::File>,
    mut output: Box<dyn Write>,
) -> Result<(), TMDLError> {
    // Build item cache
    let mut item_cache: HashMap<String, Item> = HashMap::new();
    for f in &files {
        for it in &f.items {
            item_cache.insert(it.name().to_string(), it.clone());
        }
    }

    // Collect union of operand names to understand Fields structure
    let mut operand_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for f in &files {
        for it in &f.items {
            match it {
                Item::Template(t) => {
                    for (n, _) in &t.operands {
                        operand_names.insert(n.clone());
                    }
                }
                Item::Instruction(i) => {
                    let ops = resolve_operands_for_instruction(i, &item_cache);
                    for (n, _) in ops {
                        operand_names.insert(n);
                    }
                }
                _ => {}
            }
        }
    }
    let mut operand_list: Vec<String> = operand_names.into_iter().collect();
    operand_list.sort();

    // Collect instructions
    let mut instructions = Vec::new();
    for f in &files {
        for it in &f.items {
            if let Item::Instruction(inst) = it {
                instructions.push(inst);
            }
        }
    }

    // Generate Lean file
    emit_header(&mut output)?;
    emit_state_type(&mut output)?;
    emit_fields_type(&mut output, &operand_list)?;

    for inst in &instructions {
        emit_instruction_semantics(&mut output, inst, &item_cache)?;
    }

    Ok(())
}

// Helper function to resolve operands (same as in rocqgen.rs)
fn resolve_operands_for_instruction(
    inst: &ast::Instruction,
    cache: &HashMap<String, ast::Item>,
) -> HashMap<String, ast::Type> {
    let mut result = HashMap::new();

    fn collect_from_template(
        name: &str,
        cache: &HashMap<String, ast::Item>,
        acc: &mut HashMap<String, ast::Type>,
    ) {
        if let Some(Item::Template(t)) = cache.get(name) {
            if let Some(parent) = &t.parent_template {
                collect_from_template(parent, cache, acc);
            }
            for (k, v) in &t.operands {
                acc.insert(k.clone(), v.clone());
            }
        }
    }

    if let Some(p) = &inst.parent_template {
        collect_from_template(p, cache, &mut result);
    }
    for (k, v) in &inst.operands {
        result.insert(k.clone(), v.clone());
    }
    result
}

fn emit_header(output: &mut dyn Write) -> Result<(), TMDLError> {
    writeln!(output, "-- Generated from TMDL")?;
    writeln!(output, "-- This file contains executable semantics and computational proofs")?;
    writeln!(output)?;
    writeln!(output, "import Std.Data.BitVec")?;
    writeln!(output)?;
    writeln!(output, "namespace TMDL")?;
    writeln!(output)?;
    Ok(())
}

fn emit_state_type(output: &mut dyn Write) -> Result<(), TMDLError> {
    writeln!(output, "-- Machine state")?;
    writeln!(output, "structure State where")?;
    writeln!(output, "  pc : Int")?;
    writeln!(output, "  rf : Int ’ Int  -- Register file")?;
    writeln!(output, "  deriving Repr")?;
    writeln!(output)?;
    Ok(())
}

fn emit_fields_type(output: &mut dyn Write, operand_list: &[String]) -> Result<(), TMDLError> {
    writeln!(output, "-- Instruction operand fields")?;
    writeln!(output, "structure Fields where")?;
    for operand in operand_list {
        writeln!(output, "  {} : Int", operand)?;
    }
    writeln!(output, "  deriving Repr")?;
    writeln!(output)?;
    Ok(())
}

fn emit_instruction_semantics(
    output: &mut dyn Write,
    inst: &ast::Instruction,
    item_cache: &HashMap<String, ast::Item>,
) -> Result<(), TMDLError> {
    let name = &inst.name;
    let lower_name = name.to_lowercase();

    writeln!(output, "-- Semantics for {}", name)?;
    writeln!(output, "def sem_{} (s : State) (f : Fields) : State :=", lower_name)?;

    // Get operands for this instruction
    let _operands = resolve_operands_for_instruction(inst, item_cache);

    // Generate semantics based on the behavior
    if let Some(behavior) = &inst.behavior {
        writeln!(output, "  -- TODO: Translate behavior AST to Lean")?;
        writeln!(output, "  -- Behavior: {:?}", behavior)?;
        writeln!(output, "  s  -- Placeholder: return unchanged state")?;
    } else {
        writeln!(output, "  s  -- No behavior specified")?;
    }

    writeln!(output)?;
    Ok(())
}
