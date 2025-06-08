use std::error::Error;

use tmdl::{Action, Compiler, OutputKind};

fn main() -> Result<(), Box<dyn Error>> {
    let compiler = Compiler::builder()
        .add_input("./defs/main.tmdl")
        .output(OutputKind::Batch(String::new()))
        .action(Action::EmitRust)
        .build();

    compiler.compile()
}
