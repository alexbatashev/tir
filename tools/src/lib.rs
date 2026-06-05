use std::error::Error;

use clap::{Parser, Subcommand};

mod common;

pub mod mc;

pub fn tools_main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    match cli.command {
        Command::Mc(args) => mc::run(args),
    }
}

#[derive(Subcommand)]
pub enum Command {
    Mc(mc::ToolArgs),
}

#[derive(Parser)]
#[command(name = "tir", version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
