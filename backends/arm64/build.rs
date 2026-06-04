use std::error::Error;

use tmdl::{Action, Compiler, OutputKind};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=defs");
    let compiler = Compiler::builder()
        .add_input("./defs/main.tmdl")
        .add_input("./defs/data_processing.tmdl")
        .add_input("./defs/loads_stores.tmdl")
        .add_input("./defs/branches.tmdl")
        .add_input("./defs/perf.tmdl")
        .output(OutputKind::File(format!(
            "{}/arm64.rs",
            std::env::var("OUT_DIR")?
        )))
        .dialect(Some("arm64".to_string()))
        .action(Action::EmitRust)
        .build();

    Ok(compiler.compile()?)
}
