use std::collections::{HashMap, HashSet};
use std::io::Write;

use crate::ast::{self, Instruction, Item};
use crate::error::TMDLError;
use crate::utils::resolve_operands_for_instruction;

pub fn generate_lean(
    dialect: &str,
    files: Vec<ast::File>,
    mut output: Box<dyn Write>,
) -> Result<(), TMDLError> {
    let item_cache = {
        let mut cache = HashMap::new();
        for f in &files {
            for item in &f.items {
                cache.insert(item.name().to_string(), item);
            }
        }

        cache
    };

    build_state(&files, &mut output)?;
    build_instructions(dialect, &item_cache, &files, &mut output)?;
    Ok(())
}

fn build_state(files: &[ast::File], output: &mut Box<dyn Write>) -> Result<(), TMDLError> {
    let reg_classes = files
        .iter()
        .map(|file| {
            file.items
                .iter()
                .filter_map(|item: &Item| item.as_register_class().cloned())
        })
        .flatten()
        .collect::<Vec<_>>();

    writeln!(output, "structure TMDLState where")?;
    for rc in &reg_classes {
        let name = &rc.name.to_lowercase();
        let regcount = 32;
        writeln!(output, "  {name} : Fin {regcount} -> BitVec 64")?;
    }
    writeln!(output, "  pc : BitVec 64")?;

    for rc in &reg_classes {
        let name = &rc.name.to_lowercase();
        writeln!(
            output,
            "
def TMDLState.read_{name}(st: TMDLState) (r : Nat) : BitVec 64 :=
    -- TODO correctly handle hardwired_zero registers
    if r = 0 then 0
    else if h : r < 32 then st.{name} <r, h> else 0
    else 0

def TMDLState.write_{name}(st : TMDLState) (r : Nat) (val : BitVec 64) : TMDLState :=
    -- TODO correctly handle hardwired_zero registers
    if r = 0 then st
    else if h : r < 32 then
        {{ st with {name} := Function.update st.{name} <r, h> val }}
    else
        st
"
        )?;
    }

    Ok(())
}

fn build_instructions<'a, 'cache: 'a>(
    dialect: &str,
    item_cache: &HashMap<String, &'cache Item>,
    files: &'a [ast::File],
    output: &mut Box<dyn Write>,
) -> Result<(), TMDLError> {
    let instructions = files
        .iter()
        .map(|file| {
            file.items
                .iter()
                .filter_map(|item: &Item| item.as_instruction())
        })
        .flatten()
        .collect::<Vec<_>>();

    let mut instruction_variants = vec![];
    let mut encode_arms = vec![];
    let mut execute_arms = vec![];

    for i in instructions {
        let name = i.name.to_lowercase();
        let uppercase_name = name.to_uppercase();

        let operands = resolve_operands_for_instruction(i, item_cache);

        let lean_operands = build_lean_operands(item_cache, &operands);
        let lean_encoding = build_lean_encoding(item_cache, i);
        let lean_behavior = build_lean_behavior(item_cache, i);

        let operand_list = operands
            .iter()
            .map(|(k, _v)| k.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");

        writeln!(
            output,
            "
def encode_{name} {lean_operands} : BitVec 32 :=
    {lean_encoding}

def execute_{name} (st: TMDLState) {lean_operands} : TMDLState :=
    {lean_behavior}
"
        )?;

        instruction_variants.push(format!("{uppercase_name} {lean_operands}"));
        encode_arms.push(format!(
            ".{uppercase_name} {operand_list} => encode_{name} {operand_list}"
        ));
        execute_arms.push(format!(
            ".{uppercase_name} {operand_list} => execute_{name} {operand_list}"
        ));
    }

    let instruction_variants = instruction_variants.join("\n    | ");
    let encode_arms = encode_arms.join("\n    | ");
    let execute_arms = execute_arms.join("\n    | ");
    writeln!(
        output,
        "
inductive TMDLInstr where
    | {instruction_variants}

def encode_{dialect} (instr : TMDLInstr) : BitVec 32 :=
    match instr with
    | {encode_arms}

def execute_{dialect} (state : TMDLState) (instr : TMDLInstr) : TMDLState :=
    match instr with
    | {execute_arms}
"
    )?;

    Ok(())
}

/// For a list of operands returns a string of function operands in Lean format. Examples:
/// (rd rs1 rs2 : Nat)
/// (rd rs1 : Nat) (imm : BitVec 12)
fn build_lean_operands<'cache>(
    item_cache: &HashMap<String, &'cache Item>,
    operands: &HashMap<String, ast::Type>,
) -> String {
    "()".to_string()
}

/// Builds an encoding statement from encoding description.
/// TMDL input example:
/// ```tmdl
///    encoding {
///        0..6 => OPCODE,
///        7..11 => imm[0..4],
///        12..14 => FUNCT3,
///        15..19 => rs1,
///        20..24 => rs2,
///        25..31 => imm[5..11],
///    }
/// ```
///
/// Example Lean output:
/// ```lean
/// (0b0000000 : BitVec 7) ++      -- funct7
/// (BitVec.ofNat 5 rs2) ++        -- rs2
/// (BitVec.ofNat 5 rs1) ++        -- rs1
/// (0b000 : BitVec 3) ++          -- funct3
/// (BitVec.ofNat 5 rd) ++         -- rd
/// (0b0110011 : BitVec 7)         -- opcode
/// ```
fn build_lean_encoding<'a>(
    item_cache: &HashMap<String, &'a Item>,
    instruction: &'a Instruction,
) -> String {
    "()".to_string()
}

/// Builds a function body for TMDL behavior region.
///
/// Example TMDL behavior:
/// ```tmdl
/// behavior { rd = rs1 + rs2; }
/// ```
///
/// Example corresponding Lean output:
///
/// ```lean
///   let val1 := st.read_gpr rs1
///   let val2 := st.read_gpr rs2
///   let result := val1 + val2
///   st.write_gpr rd result
/// ```
fn build_lean_behavior<'a>(
    item_cache: &HashMap<String, &'a Item>,
    instruction: &'a Instruction,
) -> String {
    "st.write_gpr x0 0".to_string()
}
