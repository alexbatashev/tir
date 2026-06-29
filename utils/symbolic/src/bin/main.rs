//! `tir-smt`: a standalone SMT-LIB solver for the QF_BV + Core subset, a fallback
//! for hosts without Z3. Reads a script from a file argument or stdin and prints
//! the responses, mirroring `z3 -smt2 <file>`.

use std::io::{self, Read, Write};
use std::process::ExitCode;

use tir_symbolic::smtlib::parser::parse_script;
use tir_symbolic::solver::run_script;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Accept and ignore option flags (e.g. `-smt2`); the first bare argument is
    // the input file, and `-` or no file means stdin.
    let file = args.iter().find(|a| !a.starts_with('-'));

    let src = match file {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot read {path}: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => {
            let mut s = String::new();
            if let Err(e) = io::stdin().read_to_string(&mut s) {
                eprintln!("error: cannot read stdin: {e}");
                return ExitCode::FAILURE;
            }
            s
        }
    };

    let script = match parse_script(&src) {
        Ok(script) => script,
        Err(errors) => {
            for e in errors {
                eprintln!("error: {e}");
            }
            return ExitCode::FAILURE;
        }
    };

    let stdout = io::stdout();
    let mut out = stdout.lock();
    if let Err(e) = run_script(&script, &mut out) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    let _ = out.flush();
    ExitCode::SUCCESS
}
