use clap::Parser;
use tmdl::{parse, tokenize};

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[arg(value_name = "FILE_NAME")]
    name: String,
}

fn main() {
    let cli = Cli::parse();

    let source = std::fs::read_to_string(&cli.name).unwrap();
    let tokens = tokenize(&source).unwrap();

    println!("{:?}", tokens);

    let tu = parse(&tokens);
    println!("{}", tu);
}
