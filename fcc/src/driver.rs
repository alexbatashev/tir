use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::lexer::Token;
use crate::preprocessor::preprocessed;

#[derive(Debug, Parser)]
#[command(name = "fcc")]
pub struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    Compile(CompileArgs),
}

#[derive(Debug, Args)]
pub struct CompileArgs {
    #[arg(long, value_enum, default_value_t = CompileStage::Preprocess)]
    stage: CompileStage,
    #[arg(short = 'o', default_value = "-")]
    output: OsString,
    inputs: Vec<OsString>,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum CompileStage {
    Preprocess,
    Ast,
    Ir,
}

pub fn compiler_main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Compile(args) => run_compile(args),
    }
}

fn run_compile(args: CompileArgs) {
    let mut out: Box<dyn Write> = if args.output == "-" {
        Box::new(BufWriter::new(io::stdout()))
    } else {
        let path = PathBuf::from(&args.output);
        Box::new(BufWriter::new(File::create(&path).unwrap_or_else(|e| {
            eprintln!(
                "fcc: cannot open output '{}': {e}",
                args.output.to_string_lossy()
            );
            std::process::exit(1);
        })))
    };

    for input in &args.inputs {
        let reader: Box<dyn io::Read> = if input == "-" {
            Box::new(io::stdin())
        } else {
            Box::new(File::open(input).unwrap_or_else(|e| {
                eprintln!("fcc: cannot open input '{}': {e}", input.to_string_lossy());
                std::process::exit(1);
            }))
        };

        match args.stage {
            CompileStage::Preprocess => {
                emit_preprocess(&mut out, preprocessed(reader, HashMap::new(), &[]));
            }
            CompileStage::Ast => {
                let unit = parse_source(reader);
                writeln!(out, "{unit:#?}").unwrap();
            }
            CompileStage::Ir => {
                let unit = parse_source(reader);
                match crate::codegen::codegen(&unit) {
                    Ok(ir) => write!(out, "{ir}").unwrap(),
                    Err(e) => {
                        eprintln!("fcc: codegen error: {e}");
                        std::process::exit(1);
                    }
                }
            }
        }
    }
}

fn parse_source(reader: Box<dyn io::Read>) -> crate::ast::TranslationUnit {
    let tokens: Vec<Token> = preprocessed(reader, HashMap::new(), &[]).collect();
    crate::parser::parse(&tokens).unwrap_or_else(|errors| {
        for e in errors {
            eprintln!("fcc: parse error: {e}");
        }
        std::process::exit(1);
    })
}

fn emit_preprocess(out: &mut dyn Write, tokens: impl Iterator<Item = Token>) {
    for tok in tokens {
        write!(out, "{tok}").unwrap();
    }
}
