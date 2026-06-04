use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};

use clap::Parser;
use tir::{Context, IRFormatter, Operation, PassManager, builtin::ModuleOp, passes::Mem2RegPass};

#[derive(Debug, Parser)]
#[command(name = "tir-opt")]
struct Cli {
    /// Pass to run. May be repeated; currently supports `mem2reg`.
    #[arg(long = "pass", short = 'p')]
    passes: Vec<String>,

    /// Output file, or `-` for stdout.
    #[arg(short = 'o', default_value = "-")]
    output: OsString,

    /// Input IR file, or `-`/omitted for stdin.
    input: Option<OsString>,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("tir-opt: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let input = read_input(cli.input.as_ref())?;

    let context = Context::with_default_dialects();
    let module = tir::parse::ir::parse_ir::<ModuleOp>(&context, &input)
        .map_err(|(span, err)| format!("failed to parse input at byte {}: {err:?}", span.0))?;

    let mut pm = PassManager::new();
    for pass in &cli.passes {
        match pass.as_str() {
            "mem2reg" => {
                pm.add_pass(Mem2RegPass::new());
            }
            other => return Err(format!("unknown pass '{other}'")),
        }
    }

    pm.run(&context, context.get_op(module.id()))
        .map_err(|e| format!("pass pipeline failed: {e}"))?;

    let mut rendered = String::new();
    let mut fmt = IRFormatter::new(&mut rendered);
    module
        .print(&mut fmt)
        .map_err(|e| format!("failed to print IR: {e}"))?;

    write_output(cli.output.as_os_str(), &rendered)
}

fn read_input(path: Option<&OsString>) -> Result<String, String> {
    let mut input = String::new();
    match path {
        None => io::stdin()
            .read_to_string(&mut input)
            .map_err(|e| format!("failed to read stdin: {e}"))?,
        Some(path) if path == "-" => io::stdin()
            .read_to_string(&mut input)
            .map_err(|e| format!("failed to read stdin: {e}"))?,
        Some(path) => File::open(path)
            .map_err(|e| format!("failed to open '{}': {e}", path.to_string_lossy()))?
            .read_to_string(&mut input)
            .map_err(|e| format!("failed to read '{}': {e}", path.to_string_lossy()))?,
    };
    Ok(input)
}

fn write_output(path: &std::ffi::OsStr, contents: &str) -> Result<(), String> {
    if path == "-" {
        print!("{contents}");
        io::stdout()
            .flush()
            .map_err(|e| format!("failed to flush stdout: {e}"))
    } else {
        std::fs::write(path, contents)
            .map_err(|e| format!("failed to write '{}': {e}", path.to_string_lossy()))
    }
}
