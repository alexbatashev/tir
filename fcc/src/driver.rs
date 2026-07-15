use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::lang_options::LangOptions;
use crate::lexer::Token;
use crate::preprocessor::preprocessed;

#[derive(Debug, Parser)]
#[command(name = "fcc")]
pub struct Cli {
    /// Print a detailed explanation of a diagnostic code, e.g. `--explain E0001`.
    #[arg(long, value_name = "CODE")]
    explain: Option<String>,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    Compile(CompileArgs),
}

#[derive(Debug, Args)]
pub struct CompileArgs {
    /// C language dialect, e.g. c17, gnu17, or c23.
    #[arg(long = "std", value_name = "STANDARD", default_value_t)]
    lang_options: LangOptions,
    #[arg(long, value_enum, default_value_t = CompileStage::Preprocess)]
    stage: CompileStage,
    /// Target architecture (required for the asm and obj stages).
    #[arg(long)]
    march: Option<String>,
    /// Target CPU
    #[arg(long)]
    mcpu: Option<String>,
    /// Target calling convention.
    #[arg(long)]
    mabi: Option<String>,
    #[arg(short = 'o', default_value = "-")]
    output: OsString,
    /// Predefine a macro, e.g. `-D NAME=VALUE` (or `-D NAME`).
    #[arg(short = 'D', value_name = "NAME[=VALUE]")]
    defines: Vec<String>,
    inputs: Vec<OsString>,
}

/// Build the predefined-macro map from `-D` arguments. Each value is lexed to a
/// single token, mirroring how `#define NAME VALUE` is stored.
fn build_defines(defines: &[String]) -> HashMap<String, Token> {
    use logos::Logos;
    defines
        .iter()
        .map(|d| {
            let (name, value) = match d.split_once('=') {
                Some((n, v)) => (n.to_string(), v.to_string()),
                None => (d.to_string(), "1".to_string()),
            };
            let tok = Token::lexer(value.trim())
                .next()
                .and_then(|r| r.ok())
                .unwrap_or(Token::Hash);
            (name, tok)
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, ValueEnum)]
pub enum CompileStage {
    /// Emit the preprocessed token stream as reconstructed source text.
    Preprocess,
    /// Emit the preprocessed token stream in its debug representation.
    Tokens,
    Ast,
    Ir,
    /// Emit textual assembly for the selected target.
    Asm,
    /// Emit an ELF relocatable object for the selected target.
    Obj,
}

pub fn compiler_main() {
    let cli = parse_cli(std::env::args_os()).unwrap_or_else(|error| error.exit());

    if let Some(code) = cli.explain {
        match crate::diagnostics::explain(&code) {
            Some(text) => print!("{text}"),
            None => {
                eprintln!("fcc: unknown diagnostic code '{code}'");
                std::process::exit(1);
            }
        }
        return;
    }

    match cli.command {
        Some(Commands::Compile(args)) => run_compile(args),
        None => {
            eprintln!("fcc: no subcommand given; try `fcc compile` or `fcc --explain <CODE>`");
            std::process::exit(1);
        }
    }
}

fn parse_cli<I, T>(args: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let args = args.into_iter().map(|arg| {
        let arg = arg.into();
        if arg == "-std" {
            OsString::from("--std")
        } else if let Some(value) = arg.to_str().and_then(|arg| arg.strip_prefix("-std=")) {
            OsString::from(format!("--std={value}"))
        } else {
            arg
        }
    });
    Cli::try_parse_from(args)
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
        let (name, source) = read_input(input);

        match args.stage {
            CompileStage::Preprocess => {
                for (tok, _) in preprocess(
                    &name,
                    &source,
                    build_defines(&args.defines),
                    args.lang_options,
                ) {
                    write!(out, "{tok}").unwrap();
                }
            }
            CompileStage::Tokens => {
                let tokens: Vec<Token> = preprocess(
                    &name,
                    &source,
                    build_defines(&args.defines),
                    args.lang_options,
                )
                .into_iter()
                .map(|(tok, _)| tok)
                .collect();
                writeln!(out, "{tokens:#?}").unwrap();
            }
            CompileStage::Ast => {
                let unit = parse_source(&name, &source, &args.defines, args.lang_options);
                write!(out, "{}", crate::ast::render(&unit)).unwrap();
            }
            CompileStage::Ir => {
                let unit = parse_source(&name, &source, &args.defines, args.lang_options);
                let context = fcc_context();
                let module = lower_to_ir(&context, unit, args.lang_options, args.march.as_deref());
                let mut ir = String::new();
                let mut fmt = tir::IRFormatter::new(&mut ir);
                use tir::Operation;
                module.print(&mut fmt).unwrap_or_else(|e| {
                    eprintln!("fcc: failed to print IR: {e}");
                    std::process::exit(1);
                });
                write!(out, "{ir}").unwrap();
            }
            CompileStage::Asm | CompileStage::Obj => {
                let bytes = emit_machine_code(&args, &name, &source);
                out.write_all(&bytes).unwrap();
            }
        }
    }
}

/// Read an input into its `(display name, source text)` pair. `-` reads stdin.
fn read_input(input: &OsString) -> (String, String) {
    if input == "-" {
        let mut source = String::new();
        io::Read::read_to_string(&mut io::stdin(), &mut source).unwrap_or_default();
        ("<stdin>".to_string(), source)
    } else {
        let source = std::fs::read_to_string(input).unwrap_or_else(|e| {
            eprintln!("fcc: cannot open input '{}': {e}", input.to_string_lossy());
            std::process::exit(1);
        });
        (input.to_string_lossy().into_owned(), source)
    }
}

fn lower_to_ir(
    context: &tir::Context,
    unit: crate::ast::Ast,
    options: LangOptions,
    march: Option<&str>,
) -> tir::builtin::ModuleOp {
    let target = match march {
        Some(march) => crate::sema::TargetProfile::for_march(march),
        None => crate::sema::TargetProfile::host(),
    }
    .unwrap_or_else(|error| {
        eprintln!("fcc: {error}; pass --march explicitly");
        std::process::exit(1);
    });
    let typed =
        crate::sema::analyze_with_target(unit, options, target).unwrap_or_else(|diagnostics| {
            for diagnostic in diagnostics {
                diagnostic.eprint();
            }
            std::process::exit(1);
        });
    crate::codegen::codegen(context, &typed).unwrap_or_else(|d| {
        d.eprint();
        std::process::exit(1);
    })
}

fn fcc_context() -> tir::Context {
    let context = tir::Context::with_default_dialects();
    context.register_dialect::<crate::cir::CirDialect>();
    context
}

fn default_include_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(sdkroot) = std::env::var("SDKROOT") {
        paths.push(PathBuf::from(sdkroot).join("usr/include"));
    }
    if let Ok(output) = std::process::Command::new("xcrun")
        .args(["--sdk", "macosx", "--show-sdk-path"])
        .output()
        && output.status.success()
    {
        let sdkroot = String::from_utf8_lossy(&output.stdout);
        paths.push(PathBuf::from(sdkroot.trim()).join("usr/include"));
    }
    paths.extend([
        PathBuf::from("/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/include"),
        PathBuf::from(
            "/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk/usr/include",
        ),
        PathBuf::from("/usr/include"),
    ]);

    let mut existing = Vec::new();
    for path in paths {
        if path.is_dir() && !existing.contains(&path) {
            existing.push(path);
        }
    }
    existing
}

/// Run the backend pipeline (mem2reg, instruction selection, register
/// allocation, finalization) and render assembly or an ELF object.
fn emit_machine_code(args: &CompileArgs, name: &str, source: &str) -> Vec<u8> {
    use tir::Operation;
    use tir::backend::pipeline::{StopAfter, build_pipeline};

    let Some(march) = args.march.as_deref() else {
        eprintln!("fcc: --march is required for the asm and obj stages");
        std::process::exit(1);
    };
    let target = tir::backend::select_target_with_abi(
        march,
        args.mcpu.as_deref(),
        None,
        args.mabi.as_deref(),
    )
    .unwrap_or_else(|e| {
        eprintln!("fcc: {e}");
        std::process::exit(1);
    });

    let unit = parse_source(name, source, &args.defines, args.lang_options);
    let context = fcc_context();
    target.register_dialects(&context);
    let module = lower_to_ir(&context, unit, args.lang_options, Some(march));

    let mut pm = tir::PassManager::new();
    pm.add_pass(crate::passes::LowerCirStructsPass::new());
    let function_pipeline = pm.nest(tir::builtin::FuncOp::name());
    function_pipeline.add_pass(crate::passes::LowerCirControlFlowPass::new());
    function_pipeline.add_pass(tir::passes::Mem2RegPass::new());
    function_pipeline.add_pass(tir::passes::InstCombinePass::new());
    function_pipeline.add_pass(tir::passes::ScfToCfgPass::new());
    let module_op = context.get_op(module.id());
    pm.run(&context, module_op.clone()).unwrap_or_else(|e| {
        eprintln!("fcc: control-flow lowering failed: {e}");
        std::process::exit(1);
    });

    crate::codegen::hoist_strings(&context, &module).unwrap_or_else(|e| {
        eprintln!("fcc: string lowering failed: {e}");
        std::process::exit(1);
    });

    let mut pm = build_pipeline(target.as_ref(), &context, StopAfter::Finalize);
    pm.run(&context, module_op).unwrap_or_else(|e| {
        eprintln!("fcc: backend pipeline failed: {e}");
        std::process::exit(1);
    });

    if args.stage == CompileStage::Asm {
        let rendered = target
            .asm_printer(&context)
            .print_module(&context, &module)
            .unwrap_or_else(|e| {
                eprintln!("fcc: failed to print assembly: {e}");
                std::process::exit(1);
            });
        return rendered.into_bytes();
    }

    let (Some(format), Some(writer)) = (target.object_format(), target.binary_writer(&context))
    else {
        eprintln!("fcc: target '{march}' does not support object emission");
        std::process::exit(1);
    };
    let object = writer
        .write_module(&context, &module, &format)
        .unwrap_or_else(|e| {
            eprintln!("fcc: failed to emit object: {e}");
            std::process::exit(1);
        });
    tir::backend::binary::write_elf(&object, &format)
}

/// Preprocess `source`, reporting any `#error`/`#warning` diagnostics. Exits if
/// any of them is an error.
fn add_default_defines(defines: &mut HashMap<String, Token>, options: LangOptions) {
    use logos::Logos;
    for (name, value) in [
        ("__GNUC__", "4"),
        ("__GNUC_MINOR__", "2"),
        ("__GNUC_PATCHLEVEL__", "1"),
        ("__APPLE__", "1"),
        ("__MACH__", "1"),
        ("__STDC__", "1"),
        ("__LP64__", "1"),
    ] {
        defines.entry(name.to_string()).or_insert_with(|| {
            Token::lexer(value)
                .next()
                .and_then(|r| r.ok())
                .unwrap_or(Token::Hash)
        });
    }
    let stdc_version = match options.std_version {
        crate::lang_options::StdVersion::C89 => None,
        crate::lang_options::StdVersion::C99 => Some("199901L"),
        crate::lang_options::StdVersion::C11 => Some("201112L"),
        crate::lang_options::StdVersion::C17 => Some("201710L"),
        crate::lang_options::StdVersion::C23 => Some("202311L"),
    };
    if let Some(value) = stdc_version {
        defines
            .entry("__STDC_VERSION__".to_string())
            .or_insert_with(|| {
                Token::lexer(value)
                    .next()
                    .and_then(|result| result.ok())
                    .unwrap()
            });
    }
    let arch_define = match std::env::consts::ARCH {
        "aarch64" => "__arm64__",
        "x86_64" => "__x86_64__",
        _ => return,
    };
    defines
        .entry(arch_define.to_string())
        .or_insert(Token::Hash);
}

fn preprocess(
    name: &str,
    source: &str,
    mut defines: HashMap<String, Token>,
    options: LangOptions,
) -> Vec<(Token, crate::diagnostics::Span)> {
    add_default_defines(&mut defines, options);
    let include_paths = default_include_paths();
    let mut stream = preprocessed(name, source, defines, &include_paths);
    let tokens = stream.collect_tokens();
    let mut had_error = false;
    for diag in stream.diagnostics() {
        diag.eprint();
        had_error |= diag.is_error();
    }
    if had_error {
        std::process::exit(1);
    }
    tokens
}

fn parse_source(
    name: &str,
    source: &str,
    defines: &[String],
    options: LangOptions,
) -> crate::ast::Ast {
    let tokens = preprocess(name, source, build_defines(defines), options);
    crate::parser::parse(&tokens, options).unwrap_or_else(|diags| {
        for diag in &diags {
            diag.eprint();
        }
        std::process::exit(1);
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{Commands, add_default_defines, parse_cli};
    use crate::lang_options::{LangOptions, StdVersion};

    #[test]
    fn accepts_gcc_attached_std_option() {
        let cli = parse_cli(["fcc", "compile", "-std=c99", "input.c"]).unwrap();
        let Some(Commands::Compile(args)) = cli.command else {
            panic!("compile command was not parsed");
        };
        assert_eq!(
            args.lang_options,
            LangOptions {
                std_version: StdVersion::C99,
                gnu_extensions: false,
            }
        );
    }

    #[test]
    fn accepts_gcc_separate_std_option() {
        assert!(parse_cli(["fcc", "compile", "-std", "c99", "input.c"]).is_ok());
    }

    #[test]
    fn accepts_long_attached_std_option() {
        assert!(parse_cli(["fcc", "compile", "--std=c99", "input.c"]).is_ok());
    }

    #[test]
    fn accepts_long_separate_std_option() {
        assert!(parse_cli(["fcc", "compile", "--std", "c99", "input.c"]).is_ok());
    }

    #[test]
    fn c89_omits_stdc_version() {
        let mut defines = HashMap::new();
        add_default_defines(
            &mut defines,
            LangOptions {
                std_version: StdVersion::C89,
                gnu_extensions: false,
            },
        );
        assert!(!defines.contains_key("__STDC_VERSION__"));
    }

    #[test]
    fn c99_sets_stdc_version() {
        let mut defines = HashMap::new();
        add_default_defines(
            &mut defines,
            LangOptions {
                std_version: StdVersion::C99,
                gnu_extensions: false,
            },
        );
        assert_eq!(defines["__STDC_VERSION__"].to_string(), "199901L");
    }
}
