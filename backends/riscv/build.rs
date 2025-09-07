use std::error::Error;

use tmdl::{Action, Compiler, OutputKind};

fn main() -> Result<(), Box<dyn Error>> {
    let compiler = Compiler::builder()
        .add_input("./defs/main.tmdl")
        .add_input("./defs/base.tmdl")
        .output(OutputKind::File(format!(
            "{}/riscv.rs",
            std::env::var("OUT_DIR")?
        )))
        .dialect(Some("riscv".to_string()))
        .action(Action::EmitRust)
        .build();

    Ok(compiler.compile()?)
}
