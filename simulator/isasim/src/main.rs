use clap::Parser;
use tir_be_common::AsmDialect;
use tir_be_common::sched::MachineModel;
use tir_riscv::RiscvDialect;
use tir_sim::timing::{self, TimingConfig};
use tir_sim::{Executor, ProgramBuilder, TraceOptions};

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
    #[arg(long, default_value_t = false)]
    trace_instructions: bool,
    #[arg(long, default_value_t = false)]
    trace_registers_each: bool,
    #[arg(long, default_value_t = false)]
    trace_registers_end: bool,
    /// Report cycle-approximate timing after the functional run.
    #[arg(long, default_value_t = false)]
    timing: bool,
    /// Machine model for `--timing`: `in-order` or `ooo`.
    #[arg(long, default_value = "ooo")]
    machine: String,
    /// Branch predictor for `--timing`: `not-taken` or `btfn`.
    #[arg(long, default_value = "btfn")]
    predictor: String,
    program: String,
}

fn select_machine(name: &str) -> Option<MachineModel> {
    match name {
        "in-order" | "inorder" => Some(tir_riscv::in_order_core_model()),
        "ooo" | "out-of-order" => Some(tir_riscv::out_of_order_core_model()),
        _ => None,
    }
}

fn main() {
    let args = Cli::parse();
    let src = std::fs::read_to_string(&args.program).expect("failed to read program path");

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

    // Pick the timing model up front so a bad `--machine` fails before running.
    let model = if args.timing {
        let m = select_machine(&args.machine).unwrap_or_else(|| {
            eprintln!(
                "unknown machine '{}' (expected: in-order, ooo)",
                args.machine
            );
            std::process::exit(2);
        });
        executor.enable_trace_recording();
        Some(m)
    } else {
        None
    };

    executor.load(program).expect("failed to load program");
    let trace = TraceOptions {
        instructions: args.trace_instructions,
        registers_after_each_instruction: args.trace_registers_each,
        registers_at_end: args.trace_registers_end,
    };
    let mut stdout = std::io::stdout();
    executor
        .run_with_trace(until_pc, args.max_cycles, trace, &mut stdout)
        .expect("program execution failed");

    if let Some(model) = model {
        let mut predictor = tir_sim::predictor::by_name(&args.predictor).unwrap_or_else(|| {
            eprintln!(
                "unknown predictor '{}' (expected: not-taken, btfn)",
                args.predictor
            );
            std::process::exit(2);
        });
        let config = TimingConfig::for_model(&model);
        let result = timing::simulate(
            &model,
            &context,
            executor.trace(),
            &config,
            predictor.as_mut(),
        );
        println!(
            "timing[{} / {}]: {} instructions, {} cycles, IPC {:.3}, {} mispredicts",
            model.name,
            predictor.name(),
            result.instructions,
            result.cycles,
            result.ipc(),
            result.mispredicts,
        );
    }
}

fn parse_addr(addr: &str) -> u64 {
    if let Some(hex) = addr.strip_prefix("0x").or_else(|| addr.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).expect("invalid hex address")
    } else {
        addr.parse::<u64>().expect("invalid decimal address")
    }
}
