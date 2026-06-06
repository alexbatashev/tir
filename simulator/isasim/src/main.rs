use clap::Parser;
use serde::{Deserialize, Serialize};
use tir::utils::APInt;
use tir_be_common::MachineContext;
use tir_sim::timing::{self, TimingConfig};
use tir_sim::{Executor, ProgramBuilder, TraceOptions};

const DEFAULT_TEST_MEMORY_OFFSET: usize = 0x1000;
const DEFAULT_TEST_MEMORY_SIZE: usize = 0x1000;
const DEFAULT_TEST_MEMORY_BASE_REG: u16 = 10; // RISC-V a0
const DEFAULT_TEST_MEMORY_ALT_REG: u16 = 11; // RISC-V a1

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
    /// JSON memory image with `regions`: `{ "regions": [{"start":"0x80001000", "hex":"efbeadde"}] }`.
    #[arg(long)]
    memory_config: Option<String>,
    /// Disable the default snippet-test memory allocation installed when no memory config is supplied.
    #[arg(long, default_value_t = false)]
    no_default_memory: bool,
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
    /// Machine model for `--timing` (target-specific, e.g. `rv64-ooo`).
    #[arg(long)]
    machine: Option<String>,
    /// Branch predictor for `--timing`: `not-taken` or `btfn`.
    #[arg(long, default_value = "btfn")]
    predictor: String,
    /// Write a structured JSON snapshot of architectural state (PC, GPRs, and any
    /// requested memory windows) to this path after the run. Used by the
    /// differential ISA test suite to compare against a golden oracle.
    #[arg(long)]
    dump_state: Option<String>,
    /// Memory window to include in `--dump-state`, formatted `addr:len` (e.g.
    /// `0x80008000:256`). Repeatable. Ignored unless `--dump-state` is set.
    #[arg(long)]
    dump_mem: Vec<String>,
    program: String,
}

/// JSON shape emitted by `--dump-state`. Hex strings keep large addresses/values
/// readable and avoid any signedness ambiguity across the oracle boundary.
#[derive(Serialize)]
struct StateDump {
    pc: String,
    /// `gprs[i]` is the value of integer register `x{i}` (unwritten reads as 0).
    gprs: Vec<String>,
    mem: Vec<MemWindowDump>,
}

#[derive(Serialize)]
struct MemWindowDump {
    addr: String,
    bytes: Vec<u8>,
}

fn main() {
    let args = Cli::parse();
    let src = std::fs::read_to_string(&args.program).expect("failed to read program path");

    let target = tir_targets::select(&args.march, args.mcpu.as_deref()).unwrap_or_else(|| {
        eprintln!(
            "unknown target '{}' (supported: {})",
            args.march,
            tir_targets::supported_targets().join(", ")
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
    let mut executor = Executor::new_at(args.mem_size, args.mem_start_address);
    if let Some(path) = &args.memory_config {
        load_memory_config(&mut executor, path);
    } else if !args.no_default_memory {
        install_default_test_memory(
            &mut executor,
            target.name(),
            args.mem_start_address,
            args.mem_size,
        );
    }

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
        let name = args.machine.as_deref().unwrap_or_else(|| {
            eprintln!(
                "--timing requires --machine (one of: {})",
                target.machines().join(", "),
            );
            std::process::exit(2);
        });
        let m = target.machine_model(name).unwrap_or_else(|| {
            eprintln!(
                "unknown machine '{}' for target '{}' (one of: {})",
                name,
                target.name(),
                target.machines().join(", "),
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

    if let Some(path) = &args.dump_state {
        write_state_dump(&executor, path, &args.dump_mem);
    }
}

/// Snapshot the final architectural state to `path` as JSON. `mem_windows` are
/// `addr:len` specs whose bytes are read out one at a time so any window that
/// runs past the configured memory simply reports a hard error rather than
/// silently truncating.
fn write_state_dump(executor: &Executor, path: &str, mem_windows: &[String]) {
    let mut gprs = Vec::with_capacity(32);
    for index in 0..32u16 {
        let value = executor
            .read_register("GPR", index)
            .expect("failed to read GPR for state dump");
        gprs.push(format!("0x{:x}", value.to_u64()));
    }

    let mut mem = Vec::with_capacity(mem_windows.len());
    for spec in mem_windows {
        let (addr, len) = parse_mem_window(spec);
        let mut bytes = Vec::with_capacity(len);
        for offset in 0..len {
            let address = addr
                .checked_add(offset as u64)
                .expect("memory window address overflow");
            let byte = executor
                .read_memory(address, 1)
                .expect("memory window does not fit configured memory window");
            bytes.push(byte as u8);
        }
        mem.push(MemWindowDump {
            addr: format!("0x{addr:x}"),
            bytes,
        });
    }

    let dump = StateDump {
        pc: format!("0x{:x}", executor.read_pc()),
        gprs,
        mem,
    };
    let json = serde_json::to_string_pretty(&dump).expect("failed to serialize state dump");
    std::fs::write(path, json).expect("failed to write state dump");
}

/// Parse a `--dump-mem` spec of the form `addr:len`, where `addr` is a hex/decimal
/// address and `len` is a byte count.
fn parse_mem_window(spec: &str) -> (u64, usize) {
    let (addr, len) = spec
        .split_once(':')
        .unwrap_or_else(|| panic!("--dump-mem expects 'addr:len', got '{spec}'"));
    let addr = parse_addr(addr.trim());
    let len = len
        .trim()
        .parse::<usize>()
        .unwrap_or_else(|_| panic!("--dump-mem length must be a byte count, got '{len}'"));
    (addr, len)
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

fn parse_addr(addr: &str) -> u64 {
    if let Some(hex) = addr.strip_prefix("0x").or_else(|| addr.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).expect("invalid hex address")
    } else {
        addr.parse::<u64>().expect("invalid decimal address")
    }
}

#[derive(Deserialize)]
struct MemoryConfig {
    #[serde(default)]
    regions: Vec<MemoryRegionConfig>,
}

#[derive(Deserialize)]
struct MemoryRegionConfig {
    start: String,
    #[serde(default)]
    bytes: Option<Vec<u8>>,
    #[serde(default)]
    hex: Option<String>,
}

fn load_memory_config(executor: &mut Executor, path: &str) {
    let text = std::fs::read_to_string(path).expect("failed to read memory config");
    let config: MemoryConfig = serde_json::from_str(&text).expect("failed to parse memory config");
    for region in config.regions {
        let start = parse_addr(&region.start);
        let bytes = match (region.bytes, region.hex) {
            (Some(bytes), None) => bytes,
            (None, Some(hex)) => parse_hex_bytes(&hex),
            (Some(_), Some(_)) => {
                panic!("memory region must specify either bytes or hex, not both")
            }
            (None, None) => panic!("memory region must specify bytes or hex"),
        };
        for (offset, byte) in bytes.into_iter().enumerate() {
            let address = start
                .checked_add(offset as u64)
                .expect("memory region address overflow");
            executor
                .write_memory(address, 1, u64::from(byte))
                .expect("memory region does not fit configured memory window");
        }
    }
}

fn install_default_test_memory(
    executor: &mut Executor,
    target: &str,
    memory_base: u64,
    memory_size: usize,
) {
    let Some((start, size)) = default_test_memory_region(memory_base, memory_size) else {
        return;
    };

    for offset in 0..size {
        let byte = (offset & 0xff) as u8;
        executor
            .write_memory(start + offset as u64, 1, u64::from(byte))
            .expect("default memory allocation must fit configured memory window");
    }

    // Project convention for quick RISC-V snippets:
    //   a0/x10 = start of the default allocation
    //   a1/x11 = midpoint, useful as a separate store destination
    if target.starts_with("riscv") || target.starts_with("rv") {
        executor
            .write_register("GPR", DEFAULT_TEST_MEMORY_BASE_REG, APInt::new(64, start))
            .expect("failed to initialize default memory base register");
        executor
            .write_register(
                "GPR",
                DEFAULT_TEST_MEMORY_ALT_REG,
                APInt::new(64, start + (size / 2) as u64),
            )
            .expect("failed to initialize default memory alternate register");
    }
}

fn default_test_memory_region(memory_base: u64, memory_size: usize) -> Option<(u64, usize)> {
    if memory_size == 0 {
        return None;
    }
    let offset = if memory_size > DEFAULT_TEST_MEMORY_OFFSET {
        DEFAULT_TEST_MEMORY_OFFSET
    } else {
        0
    };
    let size = (memory_size - offset).min(DEFAULT_TEST_MEMORY_SIZE);
    Some((memory_base + offset as u64, size))
}

fn parse_hex_bytes(hex: &str) -> Vec<u8> {
    let hex = hex
        .trim()
        .strip_prefix("0x")
        .or_else(|| hex.trim().strip_prefix("0X"))
        .unwrap_or_else(|| hex.trim());
    let mut compact = String::new();
    for ch in hex.chars() {
        if ch.is_ascii_hexdigit() {
            compact.push(ch);
        } else if ch.is_whitespace() || ch == '_' {
            continue;
        } else {
            panic!("invalid character in memory hex data");
        }
    }
    if !compact.len().is_multiple_of(2) {
        panic!("memory hex data must contain an even number of digits");
    }
    (0..compact.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&compact[i..i + 2], 16).expect("invalid memory hex byte"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_test_memory_initializes_riscv_convention() {
        let memory_base = 0x8000_0000;
        let mut executor = Executor::new_at(0x3000, memory_base);

        install_default_test_memory(&mut executor, "riscv64", memory_base, 0x3000);

        let (start, size) = default_test_memory_region(memory_base, 0x3000).unwrap();
        assert_eq!(start, 0x8000_1000);
        assert_eq!(size, DEFAULT_TEST_MEMORY_SIZE);
        assert_eq!(
            executor
                .read_register("GPR", DEFAULT_TEST_MEMORY_BASE_REG)
                .unwrap()
                .to_u64(),
            start
        );
        assert_eq!(
            executor
                .read_register("GPR", DEFAULT_TEST_MEMORY_ALT_REG)
                .unwrap()
                .to_u64(),
            start + (size / 2) as u64
        );
        assert_eq!(executor.read_memory(start, 4).unwrap(), 0x0302_0100);
    }

    #[test]
    fn default_test_memory_uses_base_when_window_is_small() {
        assert_eq!(
            default_test_memory_region(0x8000_0000, 0x800),
            Some((0x8000_0000, 0x800))
        );
    }
}
