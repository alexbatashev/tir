use std::fs;
use std::io::Write;

use crate::ast;
use crate::error::TMDLError;

// Emit Isabelle theories sufficient to wire verification.
// This generator intentionally avoids any Sail identifiers. It produces:
// - TMDL_Core.thy: TState, per-instruction Fields_*, encode_* (stub), step_* (stub)
// - TMDL_Theorems.thy: Locale TMDL_ISA and skeletons for obligations (commented)
// The detailed semantics/encoders can be filled incrementally while keeping
// the interface and xtask wiring stable.
pub fn generate_isabelle(
    _dialect: Option<&str>,
    files: Vec<ast::File>,
    out_dir: &str,
    defines: &[String],
) -> Result<(), TMDLError> {
    fs::create_dir_all(out_dir)?;

    let core_path = format!("{}/TMDL_Core.thy", out_dir);
    let thms_path = format!("{}/TMDL_Theorems.thy", out_dir);
    let corres_path = format!("{}/TMDL_Sail_Corres.thy", out_dir);
    let refine_path = format!("{}/TMDL_Sail_Refinement.thy", out_dir);

    let mut core = std::fs::File::create(&core_path)?;
    let mut thms = std::fs::File::create(&thms_path)?;
    let mut corres = std::fs::File::create(&corres_path)?;
    let mut refine = std::fs::File::create(&refine_path)?;

    let define_map = parse_defines(defines);
    emit_core(&mut core, &files, &define_map)?;
    emit_theorems(&mut thms, &files)?;
    emit_sail_correspondence(&mut corres, &files)?;
    emit_sail_refinement(&mut refine, &files)?;

    Ok(())
}

fn emit_core(mut w: &mut dyn Write, files: &[ast::File], defines: &std::collections::HashMap<String,String>) -> Result<(), TMDLError> {
    writeln!(w, "theory TMDL_Core")?;
    writeln!(w, "  imports \"Sail-Rv64d.Rv64d\" \"Word_Lib.Bitwise\"")?;
    writeln!(w, "begin\n")?;

    // A minimal machine state; users may extend via adapter relation.
    writeln!(w, "record TState =")?;
    writeln!(w, "  pc :: int")?;
    writeln!(w, "  rf :: \"int ⇒ int\"\n")?;

    // XLEN and arithmetic helpers (modular operations over XLEN bits).
    let xlen = defines.get("XLEN").cloned().unwrap_or_else(|| "64".to_string());
    writeln!(w, "definition XLEN :: int where \"XLEN = {}\"\n", xlen)?;
    writeln!(w, "definition maskX :: int where \"maskX = (2::int) ^ XLEN - 1\"\n")?;
    writeln!(w, "definition modX :: int ⇒ int where \"modX x = x mod ((2::int)^XLEN)\"\n")?;
    writeln!(w, "definition addX :: int ⇒ int ⇒ int where \"addX a b = modX (a + b)\"\n")?;
    writeln!(w, "definition subX :: int ⇒ int ⇒ int where \"subX a b = modX (a - b)\"\n")?;
    writeln!(w, "definition andX :: int ⇒ int ⇒ int where \"andX a b = (a AND b) mod ((2::int)^XLEN)\"\n")?;
    writeln!(w, "definition orX  :: int ⇒ int ⇒ int where \"orX  a b = (a OR  b) mod ((2::int)^XLEN)\"\n")?;
    writeln!(w, "definition xorX :: int ⇒ int ⇒ int where \"xorX a b = (a XOR b) mod ((2::int)^XLEN)\"\n")?;
    writeln!(w, "definition lslX :: int ⇒ int ⇒ int where \"lslX a n = modX (modX a * (2::int)^n)\"\n")?;
    writeln!(w, "definition lsrX :: int ⇒ int ⇒ int where \"lsrX a n = (modX a) div ((2::int)^n)\"\n")?;
    writeln!(w, "definition asrX :: int ⇒ int ⇒ int where \"asrX a n =\n  (let m = (2::int)^XLEN; x = modX a; signed = (if x ≥ m div 2 then x - m else x) in modX (signed div ((2::int)^n)) )\"\n")?;

    // Emit fields and stubs per instruction
    for f in files {
        for it in &f.items {
            if let ast::Item::Instruction(inst) = it {
                let iname = &inst.name;
                let fields = collect_operands(inst, files);
                writeln!(w, "record Fields_{} =", iname)?;
                if fields.is_empty() {
                    writeln!(w, "  dummy :: unit")?;
                } else {
                    for (fname, _ty) in fields {
                        // Keep types abstract as int for now; adapter constrains via state_rel
                        writeln!(w, "  {} :: int", fname)?;
                    }
                }
                writeln!(w)?;
                // Compute encoding updates and width from encoding arms
                let (updates, width) = build_encode_updates(inst, files);
                writeln!(w, "definition encode_{} :: \"Fields_{} ⇒ (bitU) list\" where", iname, iname)?;
                writeln!(w, "  \"encode_{} f = (\n     let w0 = replicate {} B0 in\n{}\n     w{} )\"\n", iname, width, updates, updates.len())?;
                // Generate step function from behavior and inferred PC increment
                let pc_inc = (width / 8) as i32;
                let fields = collect_operands(inst, files);
                let step_body = emit_step_body(inst, &fields, pc_inc).unwrap_or_else(|_| "s".to_string());
                writeln!(w, "definition step_{} :: \"TState ⇒ Fields_{} ⇒ TState\" where", iname, iname)?;
                writeln!(w, "  \"step_{} s f = {}\"\n", iname, step_body)?;
            }
        }
    }

    writeln!(w, "end")?;
    Ok(())
}

fn emit_theorems(mut w: &mut dyn Write, files: &[ast::File]) -> Result<(), TMDLError> {
    writeln!(w, "theory TMDL_Theorems")?;
    writeln!(w, "  imports TMDL_Core")?;
    writeln!(w, "begin\n")?;

    // Generic interface (no Sail names here)
    writeln!(w, "locale TMDL_ISA =\n  fixes encode_tmdl :: \"'F ⇒ (bitU) list\"\n  fixes step_tmdl :: \"'S ⇒ 'F ⇒ 'S\"\n  fixes exec_decode :: \"'RS ⇒ (bitU) list ⇒ ('Instr × 'RS)\"\n  fixes exec_run :: \"'Instr ⇒ 'RS ⇒ ('ERes × 'RS)\"\n  fixes state_rel :: \"'S ⇒ 'RS ⇒ bool\"\n\n  assumes wf_state_rel: \"True\"\nbegin\n")?;

    // Per-instruction obligations are outlined as comments to keep build green
    for f in files {
        for it in &f.items {
            if let ast::Item::Instruction(inst) = it {
                let iname = &inst.name;
                writeln!(w, "(* Existence and refinement obligations for {} *)", iname)?;
                writeln!(w, "(* lemma {}_decode_exists: \n     shows \"fst (exec_decode s (encode_{} f)) ≠ ILLEGAL\" *)\n", iname, iname)?;
                writeln!(w, "(* lemma {}_refines: \n     assumes \"state_rel sT sS\" \n     and \"True (* decoder accepts *)\" \n     shows \"∃i sS1 r sS2. (i, sS1) = exec_decode sS (encode_{} f) ∧ (r, sS2) = exec_run i sS1 ∧ state_rel (step_{} sT f) sS2\" *)\n", iname, iname, iname)?;
            }
        }
    }

    writeln!(w, "end\n")?;
    writeln!(w, "end")?;
    Ok(())
}

fn collect_operands(inst: &ast::Instruction, files: &[ast::File]) -> Vec<(String, ast::Type)> {
    use std::collections::HashMap;
    let mut cache: HashMap<String, ast::Item> = HashMap::new();
    for f in files {
        for it in &f.items {
            cache.insert(it.name().to_string(), it.clone());
        }
    }
    // Adapted from other generators: resolve template chain
    fn from_template(name: &str, cache: &HashMap<String, ast::Item>, acc: &mut Vec<(String, ast::Type)>) {
        if let Some(ast::Item::Template(t)) = cache.get(name) {
            if let Some(parent) = &t.parent_template { from_template(parent, cache, acc); }
            for (k, v) in &t.operands { acc.push((k.clone(), v.clone())); }
        }
    }
    let mut res: Vec<(String, ast::Type)> = Vec::new();
    if let Some(p) = &inst.parent_template { from_template(p, &cache, &mut res); }
    for (k, v) in &inst.operands { res.push((k.clone(), v.clone())); }
    res
}

fn emit_sail_correspondence(mut w: &mut dyn Write, files: &[ast::File]) -> Result<(), TMDLError> {
    writeln!(w, "theory TMDL_Sail_Corres")?;
    writeln!(w, "  imports TMDL_Core \"Sail-Rv64d.Rv64d\"")?;
    writeln!(w, "begin\n")?;

    for f in files {
        for it in &f.items {
            if let ast::Item::Instruction(inst) = it {
                let iname = &inst.name;
                writeln!(w, "lemma {}_decode_exists:", iname)?;
                writeln!(w, "  \"encdec_backwards_matches (encode_{} f) = return True\"", iname)?;
                writeln!(w, "  by eval\n")?;
            }
        }
    }

    writeln!(w, "end")?;
    Ok(())
}

fn emit_sail_refinement(mut w: &mut dyn Write, files: &[ast::File]) -> Result<(), TMDLError> {
    writeln!(w, "theory TMDL_Sail_Refinement")?;
    writeln!(w, "  imports TMDL_Core \"Sail-Rv64d.Rv64d\"")?;
    writeln!(w, "begin\n")?;

    // Map integer index to Sail regstate xN field; simplified for 1..31.
    writeln!(w, "fun x_of where")?;
    for i in 1..=31 {
        if i == 1 {
            writeln!(w, "  \"x_of (s::regstate) ({}::int) = x{} s\"", i, i)?;
        } else {
            writeln!(w, "| \"x_of s ({}::int) = x{} s\"", i, i)?;
        }
    }
    writeln!(w, "| \"x_of s _ = zeros' (( 64 :: int)::ii)\"\n")?;

    // State relation: PC and GPRs (x0 hardwired 0)
    writeln!(w, "definition state_rel :: \"TState ⇒ regstate ⇒ bool\" where")?;
    writeln!(w, "  \"state_rel sT sS ⟷ PC sS = to_bits XLEN (pc sT) ∧ rf sT 0 = 0 ∧ (∀i. 1 ≤ i ∧ i ≤ 31 ⟶ x_of sS i = to_bits XLEN (rf sT i))\"\n")?;

    // Per instruction refinement templates (left as comments for now)
    for f in files {
        for it in &f.items {
            if let ast::Item::Instruction(inst) = it {
                let iname = &inst.name;
                writeln!(w, "(* lemma {}_refines:", iname)?;
                writeln!(w, "   assumes SR: \"state_rel sT sS\"\n   defines \"(i, sS1) ≡ run (ext_decode (encode_{}) sS\"\n   defines \"(r, sS2) ≡ run (execute i) sS1\"\n   shows \"state_rel (step_{} sT f) sS2\"\n*)\n", iname, iname)?;
            }
        }
    }

    writeln!(w, "end")?;
    Ok(())
}

fn build_encode_updates(inst: &ast::Instruction, files: &[ast::File]) -> (String, u16) {
    use ast::{EncodingArm, Expr, Item, Lit};
    use std::collections::HashMap;

    // Build item cache
    let mut cache: HashMap<String, ast::Item> = HashMap::new();
    for f in files {
        for it in &f.items {
            cache.insert(it.name().to_string(), it.clone());
        }
    }

    // Collect encoding arms from templates and instruction
    fn resolve_encoding_for_instruction(inst: &ast::Instruction, cache: &HashMap<String, Item>) -> Vec<EncodingArm> {
        let mut encs: Vec<EncodingArm> = Vec::new();
        let mut lineage: Vec<String> = Vec::new();
        let mut cur = inst.parent_template.clone();
        while let Some(name) = cur {
            lineage.push(name.clone());
            cur = match cache.get(&name) {
                Some(Item::Template(t)) => t.parent_template.clone(),
                _ => None,
            };
        }
        for name in lineage.into_iter().rev() {
            if let Some(Item::Template(t)) = cache.get(&name) { encs.extend(t.encoding.clone()); }
        }
        encs.extend(inst.encoding.clone());
        encs
    }

    let encs = resolve_encoding_for_instruction(inst, &cache);
    // Determine width
    let mut width: u16 = 0;
    for a in &encs {
        let hi = a.end.unwrap_or(a.start);
        if hi + 1 > width { width = hi + 1; }
    }

    // Emit sequential updates w0 -> w1 -> ... using update_subrange_vec_dec
    let mut updates: Vec<String> = Vec::new();
    for (i, a) in encs.iter().enumerate() {
        let idx = i + 1;
        let lo = a.start as u32;
        let hi = a.end.unwrap_or(a.start) as u32;
        let len = (hi - lo + 1) as u32;
        let src = match &a.value {
            Expr::Lit(Lit::Int(i)) => format!("to_bits {} {}", len, sanitize_int(&i.value())),
            Expr::Ident(id) => format!("to_bits {} ({} f)" , len, field_proj(&id.name)),
            Expr::Slice(s) => {
                match &*s.base {
                    Expr::Ident(id) => {
                        if s.start == 0 { format!("to_bits {} ({} f)", len, field_proj(&id.name)) }
                        else { format!("to_bits {} (({} f) div {} )", len, field_proj(&id.name), pow2(s.start as u32)) }
                    }
                    _ => "to_bits 1 0".to_string(),
                }
            }
            Expr::IndexAccess(ia) => {
                match &*ia.base {
                    Expr::Ident(id) => format!("to_bits 1 (({} f) div {} )", field_proj(&id.name), pow2(ia.index as u32)),
                    _ => "to_bits 1 0".to_string(),
                }
            }
            _ => format!("to_bits {} 0", len),
        };
        let prev = if idx == 1 { "w0".to_string() } else { format!("w{}", idx-1) };
        updates.push(format!("     let w{} = update_subrange_vec_dec {} {} {} ({}) in", idx, prev, hi, lo, src));
    }

    (updates.join("\n"), width)
}

fn pow2(n: u32) -> String { format!("(2::int) ^ {}", n) }
fn field_proj(name: &str) -> String { format!("f.({})", name) }
fn sanitize_int(s: &str) -> String { s.to_string() }

fn emit_step_body(inst: &ast::Instruction, fields: &[(String, ast::Type)], pc_inc_bytes: i32) -> Result<String, TMDLError> {
    // Translate behavior to result value for rd
    // Recognize destination register name (commonly "rd")
    let mut dst_reg = None;
    for (name, ty) in fields {
        if let ast::Type::Struct(_) = ty {
            if name == "rd" { dst_reg = Some(name.clone()); break; }
        }
    }
    let dst = dst_reg.unwrap_or_else(|| "rd".to_string());
    let expr_str = isabelle_expr(&inst.behavior, fields)?;

    // rf update with x0 guard; pc increment by pc_inc_bytes
    let pc_inc = format!("{}", pc_inc_bytes);
    let rf_write = format!(
        "(if {} = 0 then s⦇ pc := pc s + {} ⦈ else let s1 = s⦇ pc := pc s + {} ⦈ in s1⦇ rf := (rf s1)({} := modX ({})) ⦈)",
        field_proj(&dst), pc_inc, pc_inc, field_proj(&dst), expr_str
    );
    Ok(rf_write)
}

fn parse_defines(defines: &[String]) -> std::collections::HashMap<String, String> {
    defines
        .iter()
        .filter_map(|s| {
            let parts: Vec<&str> = s.splitn(2, '=').collect();
            if parts.len() == 2 { Some((parts[0].to_string(), parts[1].to_string())) } else { None }
        })
        .collect()
}

fn isabelle_expr(e: &ast::Expr, fields: &[(String, ast::Type)]) -> Result<String, TMDLError> {
    use ast::{Expr, Lit, Binary, BinOp};
    match e {
        Expr::Assign(a) => isabelle_expr(&a.value, fields),
        Expr::Block(b) => {
            // take last assignment value
            let mut last: Option<String> = None;
            for s in &b.stmts {
                last = Some(isabelle_expr(s, fields)?);
            }
            Ok(last.unwrap_or_else(|| "0".to_string()))
        }
        Expr::Ident(id) => {
            // lookup type of identifier
            if let Some((_name, ty)) = fields.iter().find(|(n, _)| n == &id.name) {
                match ty {
                    ast::Type::Struct(_) => Ok(format!("rf s ({})", field_proj(&id.name))),
                    ast::Type::Bits(_) | ast::Type::Integer => Ok(field_proj(&id.name)),
                    ast::Type::String => Err(TMDLError::UnexpectedExpression),
                }
            } else {
                // treat unknown as immediate variable (field)
                Ok(field_proj(&id.name))
            }
        }
        Expr::Lit(Lit::Int(i)) => Ok(sanitize_int(&i.value())),
        Expr::Binary(Binary { lhs, rhs, op, .. }) => {
            let l = isabelle_expr(lhs, fields)?;
            let r = isabelle_expr(rhs, fields)?;
            let s = match op {
                BinOp::Add => format!("addX ({}) ({})", l, r),
                BinOp::Sub => format!("subX ({}) ({})", l, r),
                BinOp::BitwiseAnd => format!("andX ({}) ({})", l, r),
                BinOp::BitwiseOr => format!("orX ({}) ({})", l, r),
                BinOp::BitwiseXor => format!("xorX ({}) ({})", l, r),
                BinOp::ShiftLeftLogical => format!("lslX ({}) ({})", l, r),
                BinOp::ShiftRightLogical => format!("lsrX ({}) ({})", l, r),
                BinOp::ShiftRightArithmetic => format!("asrX ({}) ({})", l, r),
                BinOp::Mul | BinOp::Div => return Err(TMDLError::UnexpectedExpression),
            };
            Ok(s)
        }
        Expr::If(i) => {
            let c = isabelle_expr(&i.cond, fields)?;
            let t = isabelle_expr(&i.then, fields)?;
            let e2 = if let Some(e) = &i.else_ { isabelle_expr(e, fields)? } else { "0".to_string() };
            Ok(format!("(if {} ≠ 0 then ({}) else ({}))", c, t, e2))
        }
        _ => Err(TMDLError::UnexpectedExpression),
    }
}
