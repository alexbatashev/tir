use clap::Parser;
use tir_be_common::AsmDialect;
use tir_riscv::RiscvDialect;
use tir_sim::{Executor, ProgramBuilder};

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    target: String,
    #[arg(long)]
    march: Option<String>,
    #[arg(long, default_value_t = 65536)]
    mem_size: usize,
    #[arg(long, default_value_t = 0x80000000_u64)]
    mem_start_address: u64,
    #[arg(long)]
    entry: Option<String>,
    #[arg(long)]
    until_pc: String,
    #[arg(long, default_value_t = 100000)]
    max_cycles: u64,
}

fn main() {
    let args = Cli::parse();
    let src = std::fs::read_to_string(&args.target).expect("failed to read --target path");

    let context = tir::Context::with_default_dialects();
    context.register_dialect::<AsmDialect>();
    context.register_dialect::<RiscvDialect>();
    let dialect = context
        .find_dialect::<RiscvDialect>()
        .expect("failed to register riscv dialect");
    let asm_parser = dialect.get_asm_parser();
    let module = asm_parser
        .parse_asm(&context, &src)
        .expect("failed to parse assembly");

    let program = ProgramBuilder::from_module(
        &context,
        module,
        args.mem_start_address,
        args.entry.as_deref(),
    )
    .expect("failed to build program image");

    let until_pc = parse_addr(&args.until_pc);
    let mut executor = Executor::new(args.mem_size);
    executor.load(program).expect("failed to load program");
    executor
        .run(until_pc, args.max_cycles)
        .expect("program execution failed");
}

fn parse_addr(addr: &str) -> u64 {
    if let Some(hex) = addr.strip_prefix("0x").or_else(|| addr.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).expect("invalid hex address")
    } else {
        addr.parse::<u64>().expect("invalid decimal address")
    }
}
