use std::io::Read;
use std::path::PathBuf;

use clap::Parser;
use filecheck::{verify, Config, Source};

/// A standalone FileCheck-style matcher built on chumsky and ariadne.
#[derive(Debug, Parser)]
#[command(name = "filecheck", arg_required_else_help(true))]
struct Cli {
    /// File containing the check directives.
    #[arg(value_name = "CHECK-FILE")]
    check_file: PathBuf,

    /// File to verify. Defaults to standard input.
    #[arg(value_name = "INPUT-FILE", default_value = "-")]
    input_file: PathBuf,

    #[command(flatten)]
    config: Config,
}

fn read_source(path: &PathBuf) -> Source {
    if path.as_os_str() == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .unwrap_or_else(|e| {
                eprintln!("filecheck: cannot read stdin: {e}");
                std::process::exit(2);
            });
        Source::new("<stdin>", text)
    } else {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("filecheck: cannot read '{}': {e}", path.display());
            std::process::exit(2);
        });
        Source::new(path.display().to_string(), text)
    }
}

fn main() {
    let args = Cli::parse();

    let check = read_source(&args.check_file);
    let input = read_source(&args.input_file);

    match verify(&check, &input, &args.config) {
        Ok(()) => {}
        Err(diagnostic) => {
            eprint!("{diagnostic}");
            std::process::exit(1);
        }
    }
}
