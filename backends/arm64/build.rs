use std::error::Error;

use tmdl::{Action, Compiler, OutputKind};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=defs");
    let out_dir = std::env::var("OUT_DIR")?;
    let inputs = [
        "./defs/main.tmdl",
        "./defs/data_processing.tmdl",
        "./defs/loads_stores.tmdl",
        "./defs/branches.tmdl",
        "./defs/perf.tmdl",
    ];
    let compile = |action, output| {
        let mut builder = Compiler::builder()
            .output(OutputKind::File(format!("{out_dir}/{output}")))
            .dialect(Some("arm64".to_string()))
            .action(action);
        for input in inputs {
            builder = builder.add_input(input);
        }
        builder.build().compile()
    };

    compile(Action::EmitRust, "arm64.rs")?;
    compile(Action::EmitOperationList, "arm64_ops.rs")?;

    Ok(())
}
