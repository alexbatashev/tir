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
        writeln!(output, "  {name} : Fin {regcount} → BitVec 64")?;
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
    else if h : r < 32 then st.{name} ⟨r, h⟩
    else 0

def TMDLState.write_{name}(st : TMDLState) (r : Nat) (val : BitVec 64) : TMDLState :=
    -- TODO correctly handle hardwired_zero registers
    if r = 0 then st
    else if h : r < 32 then
        {{ st with {name} := fun i => if i = ⟨r, h⟩ then val else st.{name} i }}
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
            ".{uppercase_name} {operand_list} => execute_{name} state {operand_list}"
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
    deriving Repr, BEq

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
    operands: &Vec<(String, ast::Type)>,
) -> String {
    // Map a TMDL type to a Lean type string
    fn lean_ty_of(t: &ast::Type) -> String {
        match t {
            // Registers are passed as indices
            ast::Type::Struct(_) => "Nat".to_string(),
            // Bit-precise immediates
            ast::Type::Bits(w) => format!("BitVec {}", w),
            // Generic integers (signed arithmetic); if needed can be adjusted to Nat
            ast::Type::Integer => "Int".to_string(),
            ast::Type::String => "String".to_string(),
        }
    }

    if operands.is_empty() {
        return String::new();
    }

    // Group consecutive operands with the same Lean type
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for (name, ty) in operands.iter() {
        let lname = name.to_lowercase();
        let lty = lean_ty_of(ty);
        if let Some((cur_ty, names)) = groups.last_mut() {
            if *cur_ty == lty {
                names.push(lname);
                continue;
            }
        }
        groups.push((lty, vec![lname]));
    }

    // Render groups as "(a b : Ty) (c : Ty2)"
    let mut parts: Vec<String> = Vec::new();
    for (ty, names) in groups {
        parts.push(format!("({} : {})", names.join(" "), ty));
    }

    parts.join(" ")
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
    // Resolve operands for this instruction
    let operands = resolve_operands_for_instruction(instruction, item_cache)
        .into_iter()
        .collect::<HashMap<_, _>>();

    // Resolve parameters for this instruction following template chain (root-most first)
    fn resolve_params_for_instruction<'a>(
        inst: &'a ast::Instruction,
        cache: &HashMap<String, &'a ast::Item>,
    ) -> HashMap<String, (ast::Type, Option<ast::Expr>)> {
        let mut result: HashMap<String, (ast::Type, Option<ast::Expr>)> = HashMap::new();
        fn collect_from_template<'a>(
            name: &str,
            cache: &HashMap<String, &'a ast::Item>,
            acc: &mut HashMap<String, (ast::Type, Option<ast::Expr>)>,
        ) {
            if let Some(ast::Item::Template(t)) = cache.get(name) {
                if let Some(parent) = &t.parent_template {
                    collect_from_template(parent, cache, acc);
                }
                for (k, v) in &t.params {
                    acc.insert(k.clone(), v.clone());
                }
            }
        }

        if let Some(p) = &inst.parent_template {
            collect_from_template(p, cache, &mut result);
        }
        for (k, v) in &inst.params {
            result.insert(k.clone(), v.clone());
        }
        result
    }

    let params = resolve_params_for_instruction(instruction, item_cache);

    // Helper: render integer literal as BitVec of given width
    fn render_lit_bitvec(width: u16, lit: &ast::LitInt) -> String {
        let v = lit.value();
        if v.starts_with("0b") || v.starts_with("0x") || v.starts_with("0X") {
            format!("({} : BitVec {})", v, width)
        } else {
            // default to natural number literal via ofNat
            format!("(BitVec.ofNat {} {})", width, v)
        }
    }

    // Resolve encoding arms: prefer instruction's own; otherwise inherit first non-empty from templates
    let encoding_arms: Vec<ast::EncodingArm> = if !instruction.encoding.is_empty() {
        instruction.encoding.clone()
    } else {
        let mut cur = instruction.parent_template.as_ref();
        let mut out: Vec<ast::EncodingArm> = Vec::new();
        while let Some(name) = cur {
            if let Some(ast::Item::Template(t)) = item_cache.get(name.as_str()) {
                if !t.encoding.is_empty() {
                    out = t.encoding.clone();
                    break;
                }
                cur = t.parent_template.as_ref();
            } else {
                break;
            }
        }
        out
    };

    // Build each arm piece
    let mut pieces: Vec<(u16, String)> = Vec::new();
    for arm in &encoding_arms {
        let start = arm.start;
        let end = arm.end.unwrap_or(start);
        let width: u16 = end - start + 1;
        let high_bit = end; // for sorting later

        let piece = match &arm.value {
            ast::Expr::Lit(ast::Lit::Int(li)) => render_lit_bitvec(width, li),
            ast::Expr::Ident(id) => {
                let name = &id.name;
                if let Some(ty) = operands.get(name) {
                    let vname = name.to_lowercase();
                    match ty {
                        ast::Type::Struct(_) => format!("(BitVec.ofNat {} {})", width, vname),
                        ast::Type::Bits(_w) => format!("({})", vname),
                        ast::Type::Integer => format!("(BitVec.ofNat {} {})", width, vname),
                        ast::Type::String => format!("(BitVec.ofNat {} 0)", width),
                    }
                } else if let Some((pty, pval)) = params.get(name) {
                    match pval {
                        Some(ast::Expr::Lit(ast::Lit::Int(li))) => render_lit_bitvec(width, li),
                        _ => match pty {
                            ast::Type::Bits(_) | ast::Type::Integer => {
                                // Fallback if not a simple literal
                                format!("(BitVec.ofNat {} 0)", width)
                            }
                            _ => format!("(BitVec.ofNat {} 0)", width),
                        },
                    }
                } else {
                    // Unknown identifier; zero-fill
                    format!("(BitVec.ofNat {} 0)", width)
                }
            }
            ast::Expr::Slice(s) => {
                // Very simple slice rendering: prefer identifiers
                let base_str = match &*s.base {
                    ast::Expr::Ident(id) => id.name.to_lowercase(),
                    _ => "0".to_string(),
                };
                // Placeholder slice: assume LSB-first slice [start..end]
                format!("(BitVec.slice {} {} {})", base_str, s.start, s.end)
            }
            ast::Expr::IndexAccess(s) => {
                let base_str = match &*s.base {
                    ast::Expr::Ident(id) => id.name.to_lowercase(),
                    _ => "0".to_string(),
                };
                // Single bit as BitVec 1
                format!("(BitVec.slice {} {} {})", base_str, s.index, s.index)
            }
            _ => {
                // Unsupported expression in encoding; zero piece of proper width
                format!("(BitVec.ofNat {} 0)", width)
            }
        };

        pieces.push((high_bit, piece));
    }

    // Sort from highest to lowest bit-range
    pieces.sort_by(|a, b| b.0.cmp(&a.0));

    // Concatenate with ++ and indent
    let mut out = String::new();
    for (idx, (_hb, p)) in pieces.iter().enumerate() {
        if idx + 1 < pieces.len() {
            out.push_str(&format!("{} ++\n    ", p));
        } else {
            out.push_str(p);
        }
    }

    out
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
    // Resolve operands with types
    let operands = resolve_operands_for_instruction(instruction, item_cache)
        .into_iter()
        .collect::<HashMap<_, _>>();

    // Helper: map operand identifier to Lean value expression
    fn eval_expr(e: &ast::Expr, operands: &HashMap<String, ast::Type>) -> String {
        match e {
            ast::Expr::Lit(ast::Lit::Int(li)) => li.value().to_string(),
            ast::Expr::Lit(ast::Lit::Str(ls)) => format!("\"{}\"", ls.value()),
            ast::Expr::Ident(id) => {
                let name = id.name.to_lowercase();
                if let Some(ty) = operands.get(&id.name) {
                    match ty {
                        ast::Type::Struct(rc) => {
                            format!("(st.read_{} {})", rc.to_lowercase(), name)
                        }
                        _ => name,
                    }
                } else {
                    name
                }
            }
            ast::Expr::Binary(b) => {
                let lhs = eval_expr(&b.lhs, operands);
                let rhs = eval_expr(&b.rhs, operands);
                let op = match b.op {
                    ast::BinOp::Add => "+",
                    ast::BinOp::Sub => "-",
                    ast::BinOp::Mul => "*",
                    ast::BinOp::Div => "/",
                    ast::BinOp::BitwiseAnd => "&&&",
                    ast::BinOp::BitwiseOr => "|||",
                    ast::BinOp::BitwiseXor => "^^^",
                    ast::BinOp::ShiftLeftLogical => "<<<",
                    ast::BinOp::ShiftRightLogical => ">>>",
                    ast::BinOp::ShiftRightArithmetic => ">>>",
                };
                format!("({} {} {})", lhs, op, rhs)
            }
            ast::Expr::Slice(s) => {
                // Placeholder slice rendering; use base expr directly
                eval_expr(&s.base, operands)
            }
            ast::Expr::IndexAccess(s) => {
                // Placeholder index access; use base expr directly
                eval_expr(&s.base, operands)
            }
            ast::Expr::Field(f) => {
                // self.PARAM — treat as 0 for now if unknown
                if let ast::Expr::Ident(id) = &*f.base {
                    if id.name == "self" {
                        return f.member.to_lowercase();
                    }
                }
                "0".to_string()
            }
            ast::Expr::Block(b) => {
                // Evaluate last expression if marked as return; otherwise ignore
                if b.last_expr_return {
                    if let Some(last) = b.stmts.last() {
                        eval_expr(last, operands)
                    } else {
                        "0".to_string()
                    }
                } else {
                    "0".to_string()
                }
            }
            ast::Expr::Assign(a) => {
                // Not an rvalue; fallback to 0
                let _ = a;
                "0".to_string()
            }
            ast::Expr::If(i) => {
                let c = eval_expr(&i.cond, operands);
                let t = eval_expr(&i.then, operands);
                let e = if let Some(e) = &i.else_ {
                    eval_expr(e, operands)
                } else {
                    "0".to_string()
                };
                format!("(if {} then {} else {})", c, t, e)
            }
            ast::Expr::Call(_) | ast::Expr::Invalid => "0".to_string(),
        }
    }

    // Compile a statement (assignment or block) into a state-transforming Lean expr
    fn compile_to_state(
        e: &ast::Expr,
        operands: &HashMap<String, ast::Type>,
        st_name: &str,
    ) -> String {
        match e {
            ast::Expr::Assign(a) => {
                let dst_name = a.dest.to_lowercase();
                let rhs = eval_expr(&a.value, operands);
                if let Some(ast::Type::Struct(rc)) = operands.get(&a.dest) {
                    format!(
                        "({}.write_{} {} {})",
                        st_name,
                        rc.to_lowercase(),
                        dst_name,
                        rhs
                    )
                } else {
                    // Non-register destination; no state change
                    st_name.to_string()
                }
            }
            ast::Expr::Block(b) => {
                let mut current = st_name.to_string();
                for stmt in &b.stmts {
                    match stmt {
                        ast::Expr::Assign(_) | ast::Expr::Block(_) | ast::Expr::If(_) => {
                            let next = compile_to_state(stmt, operands, &current);
                            current = next;
                        }
                        _ => {}
                    }
                }
                current
            }
            ast::Expr::If(i) => {
                let cond = eval_expr(&i.cond, operands);
                let then_state = compile_to_state(&i.then, operands, st_name);
                let else_state = if let Some(e) = &i.else_ {
                    compile_to_state(e, operands, st_name)
                } else {
                    st_name.to_string()
                };
                format!("(if {} then {} else {})", cond, then_state, else_state)
            }
            _ => st_name.to_string(),
        }
    }

    // Top-level behavior
    compile_to_state(&instruction.behavior, &operands, "st")
}
