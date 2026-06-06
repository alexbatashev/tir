use std::error::Error;

use clap::{Parser, Subcommand};

mod common;

pub mod mc;
pub mod opt;
pub mod sched;

pub fn tools_main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    match cli.command {
        Command::Mc(args) => mc::run(args),
        Command::Opt(args) => opt::run(args),
        Command::Sched(args) => sched::run(args),
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// Compile machine code
    Mc(mc::ToolArgs),
    /// Run optimizations on IR
    Opt(opt::ToolArgs),
    /// Print the data dependence graph of machine IR
    Sched(sched::ToolArgs),
}

#[derive(Parser)]
#[command(name = "tir", version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
