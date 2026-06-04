use clap::Parser;
use tir_sim::timing::{self, TimingConfig};
use tir_sim::{Executor, ProgramBuilder, TraceOptions};

#[derive(Parser)]
struct Cli {
    /// Target architecture (e.g. `riscv64`, `arm64`).
    #[arg(long)]
    march: String,
    /// Target CPU. Accepted for forward compatibility; currently unused.
    #[arg(long)]
    mcpu: Option<String>,
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

fn main() {
    let args = Cli::parse();
    let src = std::fs::read_to_string(&args.program).expect("failed to read program path");

    let target = tir_targets::select(&args.march, args.mcpu.as_deref()).unwrap_or_else(|| {
        eprintln!(
            "unknown target '{}' (supported: {})",
            args.march,
            tir_targets::SUPPORTED_TARGETS.join(", ")
        );
        std::process::exit(2);
    });

    let context = tir::Context::with_default_dialects();
    target.register_dialects(&context);
    let asm_parser = target.asm_parser(&context);
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

    // `--until-pc` accepts either a symbol name or a numeric address, so tests
    // can stop at a label without hand-computing its address.
    let until_pc = resolve_pc(&args.until_pc, &program.symbols);
    let mut executor = Executor::new(args.mem_size);

    // Teach the executor which register classes share a physical file so, e.g.,
    // a value written via AArch64 `GPRsp` reads back through `GPR`.
    let register_info = target.register_info();
    let register_files = register_info
        .classes
        .iter()
        .map(|c| (c.name.to_string(), c.file.to_string()))
        .collect();
    executor.set_register_files(register_files);

    // Pick the timing model up front so a bad `--machine` fails before running.
    let model = if args.timing {
        let m = target.machine_model(&args.machine).unwrap_or_else(|| {
            eprintln!(
                "unknown machine '{}' for target '{}' (expected: in-order, ooo)",
                args.machine,
                target.name(),
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

/// Resolve a `--until-pc` argument to an address. The argument may be a `0x`
/// hex literal, a decimal address, or the name of a symbol in the program.
fn resolve_pc(arg: &str, symbols: &std::collections::BTreeMap<String, u64>) -> u64 {
    if let Some(hex) = arg.strip_prefix("0x").or_else(|| arg.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).expect("invalid hex address");
    }
    if let Ok(addr) = arg.parse::<u64>() {
        return addr;
    }
    *symbols.get(arg).unwrap_or_else(|| {
        eprintln!("--until-pc: '{arg}' is neither an address nor a known symbol");
        std::process::exit(2);
    })
}
