use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use super::actions::{DriverOptions, InputFile, StopPhase, build_actions};
use super::exec::execute;
use crate::lang_options::LangOptions;

/// fcc — a C compiler. Run `fcc cc <args>` (or invoke as `cc`/`gcc`) for the
/// gcc-compatible driver.
#[derive(Debug, Parser)]
#[command(name = "fcc")]
pub struct Cli {
    /// Print a detailed explanation of a diagnostic code, e.g. `--explain E0001`.
    #[arg(long, value_name = "CODE")]
    pub(super) explain: Option<String>,
    #[command(subcommand)]
    pub(super) command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Compile a single translation unit up to a chosen stage.
    Compile(CompileArgs),
}

/// Subcommand names routed to the native clap CLI. Kept in sync with `Commands`
/// by `known_subcommands_match_clap`.
pub(super) const KNOWN_SUBCOMMANDS: &[&str] = &["compile"];

#[derive(Debug, Args)]
pub struct CompileArgs {
    /// C language dialect, e.g. c17, gnu17, or c23.
    #[arg(long = "std", value_name = "STANDARD", default_value_t)]
    pub(super) lang_options: LangOptions,
    /// Stage to stop after and emit.
    #[arg(long, value_enum, default_value_t = CompileStage::Preprocess)]
    pub(super) stage: CompileStage,
    /// Target architecture (required for the asm and obj stages).
    #[arg(long)]
    pub(super) march: Option<String>,
    /// Target CPU
    #[arg(long)]
    pub(super) mcpu: Option<String>,
    /// Target calling convention.
    #[arg(long)]
    pub(super) mabi: Option<String>,
    /// Output file, or `-` for stdout.
    #[arg(short = 'o', default_value = "-")]
    output: OsString,
    /// Predefine a macro, e.g. `-D NAME=VALUE` (or `-D NAME`).
    #[arg(short = 'D', value_name = "NAME[=VALUE]")]
    pub(super) defines: Vec<String>,
    /// Add a directory to the include search path, e.g. `-I DIR`.
    #[arg(short = 'I', value_name = "DIR")]
    pub(super) include_dirs: Vec<PathBuf>,
    /// C source files, or `-` for stdin.
    inputs: Vec<OsString>,
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

pub(super) fn parse_cli<I, T>(args: I) -> Result<Cli, clap::Error>
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

pub(super) fn run_compile(args: CompileArgs) {
    let opts = lower(args);
    let actions = build_actions(&opts).unwrap_or_else(|e| {
        eprintln!("fcc: error: {e}");
        std::process::exit(1);
    });
    execute(&actions, &opts).unwrap_or_else(|e| {
        eprintln!("fcc: error: {e}");
        std::process::exit(1);
    });
}

pub(super) fn lower(args: CompileArgs) -> DriverOptions {
    let stop = match args.stage {
        CompileStage::Preprocess => StopPhase::Preprocess,
        CompileStage::Tokens => StopPhase::Tokens,
        CompileStage::Ast => StopPhase::Ast,
        CompileStage::Ir => StopPhase::Ir,
        CompileStage::Asm => StopPhase::Assembly,
        CompileStage::Obj => StopPhase::Object,
    };
    DriverOptions {
        inputs: args.inputs.into_iter().map(InputFile::CSource).collect(),
        output: Some(PathBuf::from(args.output)),
        stop,
        lang_options: args.lang_options,
        defines: args.defines,
        undefines: Vec::new(),
        include_dirs: args.include_dirs,
        march: args.march,
        mcpu: args.mcpu,
        mabi: args.mabi,
        lib_dirs: Vec::new(),
        libs: Vec::new(),
        dry_run: false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::actions::StopPhase;
    use super::{Commands, lower, parse_cli};
    use crate::lang_options::{LangOptions, StdVersion};

    #[test]
    fn lowers_obj_stage_to_object_phase() {
        let cli = parse_cli(["fcc", "compile", "--stage", "obj", "-o", "-", "x.c"]).unwrap();
        let Some(Commands::Compile(args)) = cli.command else {
            panic!("compile command was not parsed");
        };
        let opts = lower(args);
        assert_eq!(opts.stop, StopPhase::Object);
        assert_eq!(opts.inputs.len(), 1);
    }

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
    fn known_subcommands_match_clap() {
        use clap::CommandFactory;
        let names: Vec<String> = super::Cli::command()
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();
        assert_eq!(names, super::KNOWN_SUBCOMMANDS);
    }

    #[test]
    fn accepts_long_attached_std_option() {
        assert!(parse_cli(["fcc", "compile", "--std=c99", "input.c"]).is_ok());
    }

    #[test]
    fn accepts_long_separate_std_option() {
        assert!(parse_cli(["fcc", "compile", "--std", "c99", "input.c"]).is_ok());
    }
}
