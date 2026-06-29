//! Command-stream interpreter: runs an SMT-LIB [`Script`] against a [`Solver`]
//! and writes the responses (sat/unsat/unknown, models, values, echoes) to an
//! output sink. Declarations and assertions produce no output, matching a
//! solver with `:print-success false` — the default Z3 behaves like.

use std::io::{self, Write};

use super::Solver;
use crate::smtlib::ast::*;

/// Interpret every command in `script`, writing responses to `out`.
pub fn run_script(script: &Script, out: &mut impl Write) -> io::Result<()> {
    let mut solver = Solver::new();
    for cmd in &script.0 {
        match cmd {
            Command::SetLogic(name) => solver.set_logic(name.0.clone()),
            Command::DeclareConst(name, sort) => solver.declare_const(name.0.clone(), sort.clone()),
            Command::DeclareFun(name, args, ret) if args.is_empty() => {
                solver.declare_const(name.0.clone(), ret.clone())
            }
            Command::DefineFun(def) => solver.define_fun(def.clone()),
            Command::Assert(term) => solver.assert(term.clone()),
            Command::CheckSat => writeln!(out, "{}", solver.check_sat().as_str())?,
            Command::CheckSatAssuming(props) => {
                let extra: Vec<Term> = props.iter().map(prop_to_term).collect();
                writeln!(out, "{}", solver.check_sat_assuming(&extra).as_str())?;
            }
            Command::GetModel => match solver.get_model() {
                Some(model) => writeln!(out, "{model}")?,
                None => writeln!(out, "(error \"model is not available\")")?,
            },
            Command::GetValue(terms) => match solver.get_value(terms) {
                Some(values) => writeln!(out, "{values}")?,
                None => writeln!(out, "(error \"model is not available\")")?,
            },
            Command::Push(n) => solver.push(*n as usize),
            Command::Pop(n) => solver.pop(*n as usize),
            Command::Reset => solver.reset(),
            Command::ResetAssertions => solver.reset_assertions(),
            Command::Echo(text) => writeln!(out, "\"{text}\"")?,
            Command::Exit => break,
            // Unsupported declarations are recorded as no-ops; a check-sat that
            // depends on them reports `unknown`. Informational commands are
            // silently accepted.
            _ => {}
        }
    }
    Ok(())
}

fn prop_to_term(prop: &PropLiteral) -> Term {
    let atom = Term::Ident(QualIdentifier::Plain(Identifier::simple(
        prop.symbol.0.clone(),
    )));
    if prop.negated {
        Term::App(QualIdentifier::Plain(Identifier::simple("not")), vec![atom])
    } else {
        atom
    }
}
