use clap::Parser;

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    target: String,
    #[arg(long)]
    march: Option<String>,
    #[arg(long, default_value_t = 65536)]
    mem_size: usize,
    #[arg(long, default_value_t = 0x80000000)]
    mem_start_address: usize,
}

fn main() {
    let _args = Cli::parse();
}
