use std::error::Error;
use std::fs;
use std::io;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    // The semantic rule set is read at run time; compile it here so a typo in it
    // is a build error with a span rather than a panic in the first pass to load
    // it.
    check("defs/isel.pdl")?;

    let input = "src/passes/instcombine/rules.pdl";
    println!("cargo:rerun-if-changed={input}");
    let source = fs::read_to_string(input)?;
    let rust = match tir_pdl::compile_to_rust(&source) {
        Ok(rust) => rust,
        Err(diagnostics) => {
            let mut stderr = io::stderr().lock();
            for diagnostic in diagnostics {
                diagnostic.write(input, &source, &mut stderr)?;
            }
            return Err("failed to compile instcombine PDL rules".into());
        }
    };
    let output = PathBuf::from(std::env::var("OUT_DIR")?).join("instcombine_rules.rs");
    fs::write(output, rust)?;
    Ok(())
}

fn check(input: &str) -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed={input}");
    let source = fs::read_to_string(input)?;
    if let Err(diagnostics) = tir_pdl::compile(&source) {
        let mut stderr = io::stderr().lock();
        for diagnostic in diagnostics {
            diagnostic.write(input, &source, &mut stderr)?;
        }
        return Err(format!("failed to compile {input}").into());
    }
    Ok(())
}
