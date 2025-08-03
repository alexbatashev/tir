use std::error::Error;

use tmdl::{Action, Compiler, OutputKind};

fn main() -> Result<(), Box<dyn Error>> {
    let compiler = Compiler::builder()
        .add_input("./defs/main.tmdl")
        .add_input("./defs/base.tmdl")
        .output(OutputKind::Batch(std::env::var("OUT_DIR")?))
        .action(Action::EmitRust)
        .build();

    compiler.compile()
}
