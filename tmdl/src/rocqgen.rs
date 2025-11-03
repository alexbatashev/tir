use std::collections::{HashMap, HashSet};
use std::io::Write;

use crate::ast::{self, Instruction, Item};
use crate::error::TMDLError;
use crate::utils::resolve_operands_for_instruction;

pub fn generate_rocq(
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

    writeln!(output, "{}", HEADER)?;
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

    // Record with simple nat-indexed register files; mirrors Lean semantics
    writeln!(output, "Record TMDLState := {{")?;
    for (i, rc) in reg_classes.iter().enumerate() {
        let name = rc.name.to_lowercase();
        let sep = if i == reg_classes.len() { "" } else { ";" };
        writeln!(output, "  {} : nat -> BitVec 64{}", name, sep)?;
    }
    let sep = if reg_classes.is_empty() { "" } else { ";" };
    writeln!(output, "  pc : BitVec 64{}", sep)?;
    writeln!(output, "}}.")?;

    for rc in &reg_classes {
        let name = rc.name.to_lowercase();
        writeln!(
            output,
            "\nDefinition read_{n} (st: TMDLState) (r : nat) : BitVec 64 :=\n  if Nat.eqb r 0 then BitVec.zero 64\n  else if Nat.ltb r 32 then st.({n}) r else BitVec.zero 64.\n",
            n = name
        )?;

        // Build a record update that keeps all other fields intact
        let mut fields: Vec<String> = Vec::new();
        for rc2 in &reg_classes {
            let n2 = rc2.name.to_lowercase();
            if n2 == name {
                fields.push(format!(
                    "{} := fun i => if Nat.eqb i r then val else st.({}) i",
                    n2, n2
                ));
            } else {
                fields.push(format!("{} := st.({})", n2, n2));
            }
        }
        fields.push("pc := st.(pc)".to_string());
        writeln!(
            output,
            "Definition write_{n}(st : TMDLState) (r : nat) (val : BitVec 64) : TMDLState :=\n  if Nat.eqb r 0 then st\n  else if Nat.ltb r 32 then\n    {{| {fields} |}}\n  else st.\n",
            n = name,
            fields = fields.join("; ")
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

    for i in &instructions {
        let name = i.name.to_lowercase();
        let uppercase_name = name.to_uppercase();

        let operands = resolve_operands_for_instruction(i, item_cache);

        let coq_operands = build_coq_operands(item_cache, &operands);
        let coq_operands_ctor = build_coq_operands_ctor(item_cache, &operands);
        let coq_encoding = build_coq_encoding(item_cache, i);
        let coq_behavior = build_coq_behavior(item_cache, i);

        let operand_list = operands
            .iter()
            .map(|(k, _v)| k.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");

        writeln!(
            output,
            "\nDefinition encode_{name} {coq_operands} : BitVec 32 :=\n  {coq_encoding}.\n\nDefinition execute_{name} (st: TMDLState) {coq_operands} : TMDLState :=\n  {coq_behavior}.\n"
        )?;

        instruction_variants.push(format!(
            "| {uppercase_name} : {coq_operands_ctor} TMDLInstr"
        ));
        encode_arms.push(format!(
            "| {uppercase_name} {operand_list} => encode_{name} {operand_list}"
        ));
        execute_arms.push(format!(
            "| {uppercase_name} {operand_list} => execute_{name} state {operand_list}"
        ));
    }

    let instruction_variants = instruction_variants.join("\n  ");
    let encode_arms = encode_arms.join("\n    ");
    let execute_arms = execute_arms.join("\n    ");
    writeln!(
        output,
        "\nInductive TMDLInstr : Type :=\n  {instruction_variants}.\n\nDefinition encode_{dialect} (instr : TMDLInstr) : BitVec 32 :=\n  match instr with\n    {encode_arms}\n  end.\n\nDefinition execute_{dialect} (state : TMDLState) (instr : TMDLInstr) : TMDLState :=\n  match instr with\n    {execute_arms}\n  end.\n"
    )?;

    // ---------------------------------------------------------------------
    // A generic, enumeration-based decoder. This is intentionally simple:
    // it searches over operand domains and compares with the encoder.
    // For now, we keep it pragmatic to support RISC-V-like register operands.
    // The proofs can rely on decode (encode i) succeeding without evaluating
    // the full search space.
    // ---------------------------------------------------------------------

    // Helper: generic search combinators over finite nat ranges
    writeln!(
        output,
        "\nFixpoint search_nat (n:nat) (k:nat -> option TMDLInstr) : option TMDLInstr :=\n  match n with\n  | O => None\n  | S n' => match k n' with | Some x => Some x | None => search_nat n' k end\n  end.\n\nDefinition search_nat2 (n:nat) (k:nat -> nat -> option TMDLInstr) : option TMDLInstr :=\n  search_nat n (fun a => search_nat n (fun b => k a b)).\n\nDefinition search_nat3 (n:nat) (k:nat -> nat -> nat -> option TMDLInstr) : option TMDLInstr :=\n  search_nat n (fun a => search_nat n (fun b => search_nat n (fun c => k a b c))).\n"
    )?;

    // For each instruction, emit a "try" decoder that searches operands
    // and checks equality against the encoder.
    let mut try_decoders: Vec<String> = Vec::new();
    for i in &instructions {
        let name = i.name.to_lowercase();
        let uppercase_name = name.to_uppercase();
        let operands = resolve_operands_for_instruction(i, item_cache);

        // Build operand variable list and their domain search
        let operand_names: Vec<String> = operands
            .iter()
            .map(|(k, _)| k.to_lowercase())
            .collect();

        // For now, we only support nat operands (register indices) and BitVec immediates.
        // Registers: 0..31; BitVec w: 0..(2^w - 1). Other types are not enumerated.
        // Build the nested search function header/body.
        let mut body = String::new();
        // Build the call to encoder with operands in order
        let encoder_call = format!(
            "(encode_{name} {})",
            operand_names.join(" ")
        );
        // Comparison condition and result construction
        let some_ctor = format!(
            "Some ({ctor} {ops})",
            ctor = uppercase_name,
            ops = operand_names.join(" ")
        );
        let cond = format!(
            "if N.eqb (BitVec.val bits) (BitVec.val {enc}) then {ret} else None",
            enc = encoder_call,
            ret = some_ctor
        );

        // Determine arity to choose search depth (only up to 3 supported here)
        match operand_names.len() {
            0 => {
                body = format!("{cond}");
            }
            1 => {
                let rd = &operand_names[0];
                body = format!(
                    "search_nat 32 (fun {rd} => {cond})",
                    rd = rd,
                    cond = cond
                );
            }
            2 => {
                let rd = &operand_names[0];
                let rs1 = &operand_names[1];
                body = format!(
                    "search_nat2 32 (fun {rd} {rs1} => {cond})",
                    rd = rd,
                    rs1 = rs1,
                    cond = cond
                );
            }
            _ => {
                // 3 or more operands – limit to first three for now (covers R-type)
                let rd = &operand_names[0];
                let rs1 = &operand_names[1];
                let rs2 = &operand_names[2];
                body = format!(
                    "search_nat3 32 (fun {rd} {rs1} {rs2} => {cond})",
                    rd = rd,
                    rs1 = rs1,
                    rs2 = rs2,
                    cond = cond
                );
            }
        }

        try_decoders.push(format!(
            "Definition decode_try_{name} (bits : BitVec 32) : option TMDLInstr :=\n  {body}.",
            name = name,
            body = body
        ));
    }

    writeln!(output, "\n{}\n", try_decoders.join("\n\n"))?;

    // Aggregate decoder: try each per-instruction decoder in order using a
    // right-associated nested match to return the first successful decode.
    let mut chain = String::from("None");
    for i in instructions.iter().rev() {
        let name = i.name.to_lowercase();
        let call = format!("(decode_try_{name} bits)");
        chain = format!("match {call} with | Some x => Some x | None => {rest} end", call = call, rest = chain);
    }
    let agg = format!(
        "\nDefinition decode_{dialect} (bits : BitVec 32) : option TMDLInstr :=\n  {chain}.\n",
        dialect = dialect,
        chain = chain
    );
    writeln!(output, "{}", agg)?;

    Ok(())
}

/// For a list of operands returns a string of function operands in Coq format. Examples:
/// (rd rs1 rs2 : nat)
/// (rd rs1 : nat) (imm : BitVec 12)
fn build_coq_operands<'cache>(
    item_cache: &HashMap<String, &'cache Item>,
    operands: &Vec<(String, ast::Type)>,
) -> String {
    // Map a TMDL type to a Coq type string
    fn coq_ty_of(t: &ast::Type) -> String {
        match t {
            // Registers are passed as indices
            ast::Type::Struct(_) => "nat".to_string(),
            // Bit-precise immediates
            ast::Type::Bits(w) => format!("BitVec {}", w),
            // Generic integers (signed arithmetic)
            ast::Type::Integer => "Z".to_string(),
            ast::Type::String => "string".to_string(),
        }
    }

    if operands.is_empty() {
        return String::new();
    }

    // Group consecutive operands with the same Coq type
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for (name, ty) in operands.iter() {
        let lname = name.to_lowercase();
        let lty = coq_ty_of(ty);
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

/// Build a constructor argument list for Coq inductive: "T1 -> T2 ->"
fn build_coq_operands_ctor<'cache>(
    _item_cache: &HashMap<String, &'cache Item>,
    operands: &Vec<(String, ast::Type)>,
) -> String {
    fn coq_ty_of(t: &ast::Type) -> String {
        match t {
            ast::Type::Struct(_) => "nat".to_string(),
            ast::Type::Bits(w) => format!("BitVec {}", w),
            ast::Type::Integer => "Z".to_string(),
            ast::Type::String => "string".to_string(),
        }
    }
    let mut parts: Vec<String> = Vec::new();
    for (_name, ty) in operands.iter() {
        parts.push(format!("{} ->", coq_ty_of(ty)));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("{} ", parts.join(" "))
    }
}

fn build_coq_encoding<'a>(
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
        let decimal_value = if v.starts_with("0b") {
            // Parse binary literal
            u64::from_str_radix(&v[2..], 2).unwrap_or(0)
        } else if v.starts_with("0x") || v.starts_with("0X") {
            // Parse hex literal
            u64::from_str_radix(&v[2..], 16).unwrap_or(0)
        } else {
            // Parse decimal literal
            v.parse::<u64>().unwrap_or(0)
        };
        format!("(BitVec.of_nat {} {})", width, decimal_value)
    }

    // Resolve encoding arms similar to Lean
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

    // Build each arm piece (high-to-low) and concatenate
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
                        ast::Type::Struct(_) => format!("(BitVec.of_nat {} {})", width, vname),
                        ast::Type::Bits(_w) => format!("({})", vname),
                        ast::Type::Integer => format!("(BitVec.of_nat {} {})", width, vname),
                        ast::Type::String => format!("(BitVec.of_nat {} 0)", width),
                    }
                } else if let Some((pty, pval)) = params.get(name) {
                    match pval {
                        Some(ast::Expr::Lit(ast::Lit::Int(li))) => render_lit_bitvec(width, li),
                        _ => match pty {
                            ast::Type::Bits(_) | ast::Type::Integer => {
                                // Fallback if not a simple literal
                                format!("(BitVec.of_nat {} 0)", width)
                            }
                            _ => format!("(BitVec.of_nat {} 0)", width),
                        },
                    }
                } else {
                    // Unknown identifier; zero-fill
                    format!("(BitVec.of_nat {} 0)", width)
                }
            }
            ast::Expr::Slice(s) => {
                // Simplified: treat as zero vector of that width
                format!("(BitVec.of_nat {} 0)", width)
            }
            _ => format!("(BitVec.of_nat {} 0)", width),
        };

        pieces.push((high_bit, piece));
    }

    // Sort by decreasing high_bit and concatenate
    pieces.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out = String::new();
    for (i, (_hb, p)) in pieces.iter().enumerate() {
        if i > 0 {
            out.push_str(" ++ ");
        }
        out.push_str(p);
    }
    out
}

fn build_coq_behavior<'a>(
    item_cache: &HashMap<String, &'a Item>,
    instruction: &'a Instruction,
) -> String {
    let operands = resolve_operands_for_instruction(instruction, item_cache)
        .into_iter()
        .collect::<HashMap<_, _>>();

    // Helper: map operand identifier to Coq value expression
    fn eval_expr(e: &ast::Expr, operands: &HashMap<String, ast::Type>) -> String {
        match e {
            ast::Expr::Lit(ast::Lit::Int(li)) => li.value().to_string(),
            ast::Expr::Lit(ast::Lit::Str(ls)) => format!("\"{}\"", ls.value()),
            ast::Expr::Ident(id) => {
                let name = id.name.to_lowercase();
                if let Some(ty) = operands.get(&id.name) {
                    match ty {
                        ast::Type::Struct(rc) => {
                            format!("(read_{} st {})", rc.to_lowercase(), name)
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
                    // Keep Lean-like bitwise ops as placeholders
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
                if let ast::Expr::Ident(id) = &*f.base {
                    if id.name == "self" {
                        return f.member.to_lowercase();
                    }
                }
                "0".to_string()
            }
            ast::Expr::Block(b) => {
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
            ast::Expr::Assign(_a) => {
                // Not an rvalue; fallback to 0
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

    // Compile a statement (assignment or block) into a state-transforming Coq expr
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
                        "(write_{} {} {} {})",
                        rc.to_lowercase(),
                        st_name,
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

const HEADER: &'static str = "(* Automatically generated by TMDL compiler *)
From Stdlib Require Import NArith.

Module BitVec.
  Definition t (n:nat) := { x : N | (x < N.pow 2 (N.of_nat n))%N }.
  Coercion val {n} (w:t n) : N := proj1_sig w.
  Definition mod2n (n:nat) (x:N) := N.modulo x (N.pow 2 (N.of_nat n)).
  Lemma mod2n_bound (n : nat) (x : N) : (mod2n n x < N.pow 2 (N.of_nat n))%N.
  Proof. unfold mod2n; apply N.mod_lt; now apply N.pow_nonzero. Qed.
  Definition mk {n} (x:N) : t n := exist _ (mod2n n x) (mod2n_bound n x).

  Definition of_nat (n k:nat) : t n := mk (N.of_nat k).
  Definition zero (n:nat) : t n := mk 0%N.
  Definition add {n} (a b:t n) : t n := mk (a + b).
  Definition sub {n} (a b:t n) : t n := mk (N.sub a b).
  Definition land {n} (a b:t n) : t n := mk (N.land a b).
  Definition lor  {n} (a b:t n) : t n := mk (N.lor  a b).
  Definition lxor {n} (a b:t n) : t n := mk (N.lxor a b).
  Definition shl  {n} (a b:t n) : t n := mk (N.shiftl a b).
  Definition concat {n m} (a:t n) (b:t m) : t (n+m) := mk (a * N.pow 2 (N.of_nat m) + b).
End BitVec.

Declare Scope bv_scope.

Infix \"+\"   := BitVec.add (at level 50, left associativity) : bv_scope.
Infix \"-\"   := BitVec.sub (at level 50, left associativity) : bv_scope.
Infix \"^^^\" := BitVec.lxor (at level 40, left associativity) : bv_scope.
Infix \"|||\" := BitVec.lor (at level 40, left associativity) : bv_scope.
Infix \"&&&\" := BitVec.land (at level 40, left associativity) : bv_scope.
Infix \"<<<\" := BitVec.shl (at level 35, no associativity) : bv_scope.
Infix \"++\"  := BitVec.concat (at level 60, right associativity) : bv_scope.
Open Scope bv_scope.

Notation BitVec n := (BitVec.t n).
";
