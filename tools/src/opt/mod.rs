use std::{error::Error, ffi::OsString};

use clap::Args;
use tir::{Context, IRFormatter, Operation, PassManager, builtin::ModuleOp, passes::Mem2RegPass};

use crate::common::{read_input, write_output};

#[derive(Args)]
pub struct ToolArgs {
    /// Pass to run. May be repeated; currently supports `mem2reg`.
    #[arg(long = "pass", short = 'p')]
    passes: Vec<String>,

    /// Output file, or `-` for stdout.
    #[arg(short = 'o', default_value = "-")]
    output: OsString,

    /// Input IR file, or `-`/omitted for stdin.
    input: Option<OsString>,
}

pub fn run(args: ToolArgs) -> Result<(), Box<dyn Error>> {
    let input = read_input(args.input.as_ref())?;

    let context = Context::with_default_dialects();
    let module = tir::parse::ir::parse_ir::<ModuleOp>(&context, &input)
        .map_err(|(span, err)| format!("failed to parse input at byte {}: {err:?}", span.0))?;

    let mut pm = PassManager::new();
    for pass in &args.passes {
        match pass.as_str() {
            "mem2reg" => {
                pm.add_pass(Mem2RegPass::new());
            }
            other => return Err(format!("unknown pass '{other}'").into()),
        }
    }
    pm.run(&context, context.get_op(module.id()))
        .map_err(|e| format!("pass pipeline failed: {e}"))?;

    let mut rendered = String::new();
    let mut fmt = IRFormatter::new(&mut rendered);
    module
        .print(&mut fmt)
        .map_err(|e| format!("failed to print IR: {e}"))?;

    write_output(args.output.as_os_str(), &rendered).map_err(|e| e.into())
}
