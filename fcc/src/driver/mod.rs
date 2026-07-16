mod actions;
mod cli;
mod compile;
mod exec;
mod gcc;

use std::ffi::OsString;
use std::path::Path;

use cli::{Commands, KNOWN_SUBCOMMANDS, parse_cli, run_compile};

#[derive(Debug, PartialEq, Eq)]
enum Route {
    Native,
    Gcc { skip: usize },
}

pub fn compiler_main() {
    let argv: Vec<OsString> = std::env::args_os().collect();
    let base = program_base(&argv);
    let first = argv.get(1).and_then(|a| a.to_str());
    match route(&base, first) {
        Route::Gcc { skip } => gcc::run_gcc(argv.into_iter().skip(1 + skip)),
        Route::Native => run_native(argv),
    }
}

fn program_base(argv: &[OsString]) -> String {
    argv.first()
        .map(Path::new)
        .and_then(Path::file_name)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn route(argv0_base: &str, first: Option<&str>) -> Route {
    if argv0_base == "cc" || argv0_base == "gcc" {
        return Route::Gcc { skip: 0 };
    }
    match first {
        None => Route::Native,
        Some("cc") => Route::Gcc { skip: 1 },
        Some(a)
            if KNOWN_SUBCOMMANDS.contains(&a)
                || matches!(a, "--explain" | "--help" | "-h" | "--version") =>
        {
            Route::Native
        }
        Some(_) => Route::Gcc { skip: 0 },
    }
}

fn run_native(argv: Vec<OsString>) {
    let cli = parse_cli(argv).unwrap_or_else(|error| error.exit());

    if let Some(code) = cli.explain {
        match crate::diagnostics::explain(&code) {
            Some(text) => print!("{text}"),
            None => {
                eprintln!("fcc: error: unknown diagnostic code '{code}'");
                std::process::exit(1);
            }
        }
        return;
    }

    match cli.command {
        Some(Commands::Compile(args)) => run_compile(args),
        None => {
            eprintln!(
                "fcc: error: no subcommand given; try `fcc compile` or `fcc --explain <CODE>`"
            );
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Route, route};

    #[test]
    fn argv0_gcc_routes_to_gcc_mode() {
        assert_eq!(route("gcc", Some("-c")), Route::Gcc { skip: 0 });
    }

    #[test]
    fn argv0_cc_routes_to_gcc_mode() {
        assert_eq!(route("cc", None), Route::Gcc { skip: 0 });
    }

    #[test]
    fn cc_subcommand_skips_the_cc_arg() {
        assert_eq!(route("fcc", Some("cc")), Route::Gcc { skip: 1 });
    }

    #[test]
    fn known_subcommand_routes_native() {
        assert_eq!(route("fcc", Some("compile")), Route::Native);
    }

    #[test]
    fn explain_routes_native() {
        assert_eq!(route("fcc", Some("--explain")), Route::Native);
    }

    #[test]
    fn no_args_routes_native() {
        assert_eq!(route("fcc", None), Route::Native);
    }

    #[test]
    fn bare_gcc_flag_falls_through_to_gcc_mode() {
        assert_eq!(route("fcc", Some("-c")), Route::Gcc { skip: 0 });
    }
}
