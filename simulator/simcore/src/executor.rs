use std::collections::{BTreeMap, HashMap};
use std::io::Write;

use tir::attributes::AttributeValue;
use tir::builtin::ModuleOp;
use tir::{Context, Operation};
use tir_be_common::{MachineContext, MachineInstruction, SectionOp, SimTrap, SymbolOp};

use crate::{MachineBlock, error::Error};

#[derive(Clone)]
pub struct ProgramImage {
    pub context: Context,
    pub entry_pc: u64,
    pub symbols: BTreeMap<String, u64>,
    pub blocks: Vec<MachineBlock>,
    pub block_map_by_start: HashMap<u64, usize>,
}

#[derive(Debug, Clone, Copy)]
pub struct ProgramBuilder {
    pub base_address: u64,
}

impl ProgramBuilder {
    pub fn from_module(
        context: &Context,
        module: ModuleOp,
        base_address: u64,
        entry_symbol: Option<&str>,
    ) -> Result<ProgramImage, Error> {
        let mut symbols = BTreeMap::new();
        let mut blocks = Vec::new();
        let mut cur_pc = base_address;
        let mut first_symbol_pc = None;

        let mut blocks_to_visit = vec![module.body()];
        while let Some(block) = blocks_to_visit.pop() {
            for op_id in block.op_ids() {
                let op = context.get_op(op_id);

                if let Some(section) = op.clone().as_op::<SectionOp>() {
                    blocks_to_visit.push(section.body());
                    continue;
                }

                let symbol = op.as_op::<SymbolOp>();

                if symbol.is_none() {
                    continue;
                }

                let symbol = symbol.unwrap();

                let symbol_name = symbol
                    .attributes()
                    .iter()
                    .find_map(|attr| {
                        if attr.name != "name" {
                            return None;
                        }
                        match &attr.value {
                            AttributeValue::Str(s) => Some(s.clone()),
                            _ => None,
                        }
                    })
                    .ok_or(Error::MissingSymbolName)?;
                symbols.insert(symbol_name, cur_pc);
                if first_symbol_pc.is_none() {
                    first_symbol_pc = Some(cur_pc);
                }

                let symbol_block = symbol.body();
                let mut instruction_ops = Vec::new();
                let mut block_len = 0u64;

                for inner_id in symbol_block.op_ids() {
                    let inner_op = context.get_op(inner_id);
                    if let Some(machine_inst) = inner_op.as_interface::<dyn MachineInstruction>() {
                        instruction_ops.push(inner_id);
                        block_len += u64::from(machine_inst.width_bytes());
                    }
                }

                blocks.push(MachineBlock {
                    block: symbol_block.id(),
                    instructions: instruction_ops,
                    start_address: cur_pc,
                    byte_len: block_len,
                    fallthrough_pc: None,
                });
                cur_pc += block_len.max(4);
            }
        }

        if blocks.is_empty() {
            return Err(Error::NoSymbolsFound);
        }

        for i in 0..blocks.len() {
            if i + 1 < blocks.len() {
                blocks[i].fallthrough_pc = Some(blocks[i + 1].start_address);
            }
        }

        let entry_pc = if let Some(entry_name) = entry_symbol {
            *symbols
                .get(entry_name)
                .ok_or_else(|| Error::EntrySymbolNotFound(entry_name.to_string()))?
        } else {
            first_symbol_pc.ok_or(Error::NoSymbolsFound)?
        };

        let block_map_by_start = blocks
            .iter()
            .enumerate()
            .map(|(idx, block)| (block.start_address, idx))
            .collect();

        Ok(ProgramImage {
            context: context.clone(),
            entry_pc,
            symbols,
            blocks,
            block_map_by_start,
        })
    }
}

#[derive(Default)]
pub struct Executor {
    program: Option<ProgramImage>,
    registers: HashMap<(String, u16), tir::utils::APInt>,
    /// Map from register class name to its physical register file. Classes that
    /// share a file (e.g. AArch64 `GPR` and `GPRsp`) alias index-for-index, so
    /// register storage is keyed by file rather than by class. Classes absent
    /// from the map are their own file.
    register_files: HashMap<String, String>,
    memory: Vec<u8>,
    pc: u64,
    pc_explicitly_written: bool,
    record_trace: bool,
    trace: Vec<(tir::OpId, u64)>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TraceOptions {
    pub instructions: bool,
    pub registers_after_each_instruction: bool,
    pub registers_at_end: bool,
}

impl Executor {
    pub fn new(memory_size: usize) -> Self {
        Self {
            memory: vec![0u8; memory_size],
            ..Self::default()
        }
    }

    pub fn load(&mut self, program: ProgramImage) -> Result<(), Error> {
        if self.program.is_some() {
            return Err(Error::ProgramAlreadyLoaded);
        }
        self.pc = program.entry_pc;
        self.program = Some(program);
        Ok(())
    }

    /// Record the dynamic instruction stream (the executed op ids, in order) so a
    /// timing model can replay it. Off by default to avoid the memory cost.
    pub fn enable_trace_recording(&mut self) {
        self.record_trace = true;
    }

    /// Declare which register classes share a physical register file (class name
    /// -> file name). With this set, a value written through one class is
    /// visible through any aliasing class, matching real hardware (e.g. AArch64
    /// `GPR`/`GPRsp`). Without it, each class is its own independent file.
    pub fn set_register_files(&mut self, register_files: HashMap<String, String>) {
        self.register_files = register_files;
    }

    /// Canonicalize a register class to the physical file it draws from.
    fn register_file<'a>(&'a self, class: &'a str) -> &'a str {
        self.register_files
            .get(class)
            .map(String::as_str)
            .unwrap_or(class)
    }

    /// The recorded dynamic instruction stream as `(op, pc)` pairs, in execution
    /// order. The PC lets a timing model reconstruct branch directions/outcomes.
    pub fn trace(&self) -> &[(tir::OpId, u64)] {
        &self.trace
    }

    pub fn run(&mut self, until_pc: u64, max_cycles: u64) -> Result<(), Error> {
        let mut sink = std::io::sink();
        self.run_with_trace(until_pc, max_cycles, TraceOptions::default(), &mut sink)
    }

    pub fn run_with_trace(
        &mut self,
        until_pc: u64,
        max_cycles: u64,
        trace: TraceOptions,
        out: &mut dyn Write,
    ) -> Result<(), Error> {
        for _cycle in 0..max_cycles {
            if self.pc == until_pc {
                if trace.registers_at_end {
                    self.emit_register_dump(out, "final registers");
                }
                return Ok(());
            }

            let (instructions, fallthrough_pc, context) = {
                let program = self.program.as_ref().ok_or(Error::ProgramNotLoaded)?;
                let idx = *program
                    .block_map_by_start
                    .get(&self.pc)
                    .ok_or(SimTrap::PcNotMapped { pc: self.pc })?;
                let block = &program.blocks[idx];
                (
                    block.instructions.clone(),
                    block.fallthrough_pc,
                    program.context.clone(),
                )
            };

            self.pc_explicitly_written = false;
            let mut inst_pc = self.pc;
            for op_id in instructions {
                if inst_pc == until_pc {
                    self.pc = inst_pc;
                    if trace.registers_at_end {
                        self.emit_register_dump(out, "final registers");
                    }
                    return Ok(());
                }
                let op = context.get_op(op_id);
                let machine_inst = op
                    .clone()
                    .as_interface::<dyn MachineInstruction>()
                    .ok_or_else(|| SimTrap::InvalidInstruction {
                        op: op.name,
                        reason: "operation does not implement MachineInstruction".to_string(),
                    })?;
                if trace.instructions {
                    let line = format!(
                        "pc=0x{inst_pc:016x}  {}",
                        Self::format_instruction_line(&context, &op, machine_inst.as_ref())
                    );
                    Self::emit_trace_line(out, &line);
                }
                if self.record_trace {
                    self.trace.push((op_id, inst_pc));
                }
                // Expose this instruction's own address so PC-relative semantics
                // (`PC::pc`) resolve correctly even mid-block.
                self.pc = inst_pc;
                self.pc_explicitly_written = false;
                machine_inst.execute(self)?;
                if trace.registers_after_each_instruction {
                    self.emit_register_dump(out, "registers");
                }
                if self.pc_explicitly_written {
                    // A control transfer wrote PC: `self.pc` holds the target, and
                    // the next block is resolved at the top of the outer loop.
                    break;
                }
                inst_pc = inst_pc.wrapping_add(u64::from(machine_inst.width_bytes()));
            }

            if !self.pc_explicitly_written {
                if let Some(next_pc) = fallthrough_pc {
                    self.pc = next_pc;
                } else {
                    if trace.registers_at_end {
                        self.emit_register_dump(out, "final registers");
                    }
                    return Err(Error::MissingFallthrough { pc: inst_pc });
                }
            }
        }

        if trace.registers_at_end {
            self.emit_register_dump(out, "final registers");
        }
        Err(SimTrap::MaxCyclesExceeded {
            max_cycles,
            until_pc,
        }
        .into())
    }

    pub fn register_snapshot(&self) -> Vec<(String, u16, tir::utils::APInt)> {
        let mut regs = self
            .registers
            .iter()
            .map(|((class, idx), value)| (class.clone(), *idx, value.clone()))
            .collect::<Vec<_>>();
        regs.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        regs
    }

    fn format_instruction_line(
        context: &Context,
        op: &std::sync::Arc<tir::OpInstance>,
        machine_inst: &dyn MachineInstruction,
    ) -> String {
        let mut pieces = Vec::new();
        for attr in &op.attributes {
            let mut value_buf = String::new();
            let mut formatter = tir::IRFormatter::new(&mut value_buf);
            if attr.value.print(&mut formatter, context).is_ok() {
                pieces.push(format!("{}={}", attr.name, value_buf));
            } else {
                pieces.push(format!("{}=<print-error>", attr.name));
            }
        }
        if pieces.is_empty() {
            machine_inst.mnemonic().to_string()
        } else {
            format!("{} {}", machine_inst.mnemonic(), pieces.join(", "))
        }
    }

    fn emit_register_dump(&self, out: &mut dyn Write, label: &str) {
        let snapshot = self.register_snapshot();
        Self::emit_trace_line(out, &format!("{label}:"));
        if snapshot.is_empty() {
            Self::emit_trace_line(out, "  <none>");
            return;
        }
        for (class, index, value) in snapshot {
            Self::emit_trace_line(
                out,
                &format!(
                    "  {}[{}] = 0x{:x} (width={})",
                    class,
                    index,
                    value.to_u64(),
                    value.width()
                ),
            );
        }
    }

    fn emit_trace_line(out: &mut dyn Write, line: &str) {
        let _ = writeln!(out, "{line}");
    }
}

impl MachineContext for Executor {
    fn read_register(&self, class: &str, index: u16) -> Result<tir::utils::APInt, SimTrap> {
        // The program counter is held specially (it drives instruction fetch), but
        // semantics reference it as the `PC` register class (e.g. `PC::pc`).
        if class == "PC" {
            return Ok(tir::utils::APInt::new(64, self.pc));
        }
        let key = (self.register_file(class).to_string(), index);
        if let Some(value) = self.registers.get(&key) {
            return Ok(value.clone());
        }
        Ok(tir::utils::APInt::new(64, 0))
    }

    fn write_register(
        &mut self,
        class: &str,
        index: u16,
        value: tir::utils::APInt,
    ) -> Result<(), SimTrap> {
        if class == "PC" {
            self.write_pc(value.to_u64());
            return Ok(());
        }
        let file = self.register_file(class).to_string();
        self.registers.insert((file, index), value);
        Ok(())
    }

    fn read_memory(&self, address: u64, size: usize) -> Result<u64, SimTrap> {
        let start = usize::try_from(address).map_err(|_| SimTrap::BadAddress { address, size })?;
        let end = start
            .checked_add(size)
            .ok_or(SimTrap::BadAddress { address, size })?;
        if end > self.memory.len() {
            return Err(SimTrap::BadAddress { address, size });
        }
        let mut value = 0u64;
        for (offset, byte) in self.memory[start..end].iter().enumerate() {
            value |= u64::from(*byte) << (offset * 8);
        }
        Ok(value)
    }

    fn write_memory(&mut self, address: u64, size: usize, value: u64) -> Result<(), SimTrap> {
        let start = usize::try_from(address).map_err(|_| SimTrap::BadAddress { address, size })?;
        let end = start
            .checked_add(size)
            .ok_or(SimTrap::BadAddress { address, size })?;
        if end > self.memory.len() {
            return Err(SimTrap::BadAddress { address, size });
        }
        for offset in 0..size {
            self.memory[start + offset] = ((value >> (offset * 8)) & 0xFF) as u8;
        }
        Ok(())
    }

    fn read_pc(&self) -> u64 {
        self.pc
    }

    fn write_pc(&mut self, value: u64) {
        self.pc = value;
        self.pc_explicitly_written = true;
    }
}

#[cfg(test)]
mod tests {
    use tir::Context;
    use tir::utils::APInt;
    use tir_be_common::{AsmDialect, MachineInstruction};
    use tir_riscv::RiscvDialect;

    use crate::{Executor, ProgramBuilder, TraceOptions, error::Error};

    #[test]
    fn run_stops_before_until_pc() {
        let context = Context::with_default_dialects();
        context.register_dialect::<AsmDialect>();
        context.register_dialect::<RiscvDialect>();

        let dialect = context.find_dialect::<RiscvDialect>().unwrap();
        let asm = "
            .global first
            first:
              add x1, x1, x1
            .global second
            second:
              add x2, x2, x2
        ";
        let module = dialect.get_asm_parser().parse_asm(&context, asm).unwrap();
        let program = ProgramBuilder::from_module(&context, module, 0x8000_0000, Some("first"))
            .expect("program builder must succeed");

        let until_pc = program.entry_pc;
        let mut executor = Executor::new(4096);
        tir_be_common::MachineContext::write_register(&mut executor, "GPR", 1, APInt::new(64, 3))
            .unwrap();
        executor.load(program).unwrap();
        executor.run(until_pc, 10).unwrap();

        let x1 = tir_be_common::MachineContext::read_register(&executor, "GPR", 1).unwrap();
        let x2 = tir_be_common::MachineContext::read_register(&executor, "GPR", 2).unwrap();
        assert_eq!(x1.to_u64(), 3);
        assert_eq!(x2.to_u64(), 0);
        assert_eq!(tir_be_common::MachineContext::read_pc(&executor), until_pc);
    }

    #[test]
    fn run_traps_when_max_cycles_exhausted() {
        let context = Context::with_default_dialects();
        context.register_dialect::<AsmDialect>();
        context.register_dialect::<RiscvDialect>();

        let dialect = context.find_dialect::<RiscvDialect>().unwrap();
        let asm = "
            .global first
            first:
              add x1, x1, x1
        ";
        let module = dialect.get_asm_parser().parse_asm(&context, asm).unwrap();
        let program =
            ProgramBuilder::from_module(&context, module, 0x8000_0000, Some("first")).unwrap();
        let mut executor = Executor::new(4096);
        executor.load(program).unwrap();

        let err = executor.run(0xFFFF_FFFF, 0).unwrap_err();
        match err {
            Error::Trap(tir_be_common::SimTrap::MaxCyclesExceeded { .. }) => {}
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[test]
    fn run_keeps_hardwired_zero_register_immutable() {
        let context = Context::with_default_dialects();
        context.register_dialect::<AsmDialect>();
        context.register_dialect::<RiscvDialect>();

        let dialect = context.find_dialect::<RiscvDialect>().unwrap();
        let asm = "
            .global first
            first:
              add x0, x1, x1
        ";
        let module = dialect.get_asm_parser().parse_asm(&context, asm).unwrap();
        let program = ProgramBuilder::from_module(&context, module, 0x8000_0000, Some("first"))
            .expect("program builder must succeed");
        let mut executor = Executor::new(4096);
        tir_be_common::MachineContext::write_register(&mut executor, "GPR", 1, APInt::new(64, 7))
            .unwrap();

        let inst_id = *program
            .blocks
            .first()
            .and_then(|block| block.instructions.first())
            .expect("program should contain one machine instruction");
        let inst_op = context.get_op(inst_id);
        let machine_inst = inst_op
            .clone()
            .as_interface::<dyn MachineInstruction>()
            .expect("expected machine instruction in symbol body");
        machine_inst.execute(&mut executor).unwrap();

        let x0 = tir_be_common::MachineContext::read_register(&executor, "GPR", 0).unwrap();
        assert_eq!(x0.to_u64(), 0);
    }

    #[test]
    fn run_with_trace_emits_instruction_and_registers() {
        let context = Context::with_default_dialects();
        context.register_dialect::<AsmDialect>();
        context.register_dialect::<RiscvDialect>();

        let dialect = context.find_dialect::<RiscvDialect>().unwrap();
        let asm = "
            .global first
            first:
              add x1, x1, x1
            .global second
            second:
              add x2, x2, x2
        ";
        let module = dialect.get_asm_parser().parse_asm(&context, asm).unwrap();
        let program = ProgramBuilder::from_module(&context, module, 0x8000_0000, Some("first"))
            .expect("program builder must succeed");
        let mut executor = Executor::new(4096);
        tir_be_common::MachineContext::write_register(&mut executor, "GPR", 1, APInt::new(64, 2))
            .unwrap();
        executor.load(program).unwrap();

        let mut trace_output = Vec::new();
        let err = executor
            .run_with_trace(
                u64::MAX,
                1,
                TraceOptions {
                    instructions: true,
                    registers_after_each_instruction: true,
                    registers_at_end: true,
                },
                &mut trace_output,
            )
            .unwrap_err();
        match err {
            Error::Trap(tir_be_common::SimTrap::MaxCyclesExceeded { .. }) => {}
            Error::MissingFallthrough { .. } => {}
            other => panic!("unexpected error: {:?}", other),
        }

        let trace_text = String::from_utf8(trace_output).unwrap();
        assert!(trace_text.contains("pc=0x"));
        assert!(trace_text.contains("add"));
        assert!(trace_text.contains("registers:"));
        assert!(trace_text.contains("final registers:"));
    }

    /// `cmp` writes all four AArch64 condition flags (`PSTATE` n/z/c/v), and a
    /// conditional branch reads them back. Both used to be silently dropped: the
    /// multi-assignment behaviors only emitted one write (or none), and flag paths
    /// could not be lowered at all. Flags live in a register class with index-less
    /// registers, so this also exercises the canonical-index support that ports to
    /// any target with status/flag registers.
    #[test]
    fn arm64_compare_sets_flags_and_conditional_branch_reads_them() {
        use tir::Operation;
        use tir::attributes::{AttributeValue, RegisterAttr};
        use tir_be_common::{MachineContext, MachineInstruction};

        fn gpr(index: u16) -> AttributeValue {
            AttributeValue::Register(RegisterAttr::Physical {
                class: "GPR".to_string(),
                index,
            })
        }

        // PSTATE flag slots, assigned by declaration order in the register class.
        const N: u16 = 0;
        const Z: u16 = 1;
        const C: u16 = 2;
        const V: u16 = 3;

        let context = Context::with_default_dialects();
        context.register_dialect::<tir_be_common::AsmDialect>();
        context.register_dialect::<arm64::Arm64Dialect>();

        let exec_cmp = |x0: u64, x1: u64| -> Executor {
            let mut ex = Executor::new(64);
            MachineContext::write_register(&mut ex, "GPR", 0, APInt::new(64, x0)).unwrap();
            MachineContext::write_register(&mut ex, "GPR", 1, APInt::new(64, x1)).unwrap();
            let cmp = arm64::CompareOpBuilder::new(&context)
                .attr("rn", gpr(0))
                .attr("rm", gpr(1))
                .build();
            let mi = context
                .get_op(cmp.id())
                .as_interface::<dyn MachineInstruction>()
                .expect("cmp is a machine instruction");
            mi.execute(&mut ex).expect("cmp executes");
            ex
        };
        let flag = |ex: &Executor, idx: u16| {
            MachineContext::read_register(ex, "PSTATE", idx)
                .unwrap()
                .to_u64()
        };

        // Equal operands: Z and C set, N and V clear.
        let eq = exec_cmp(5, 5);
        assert_eq!(flag(&eq, Z), 1, "Z set when operands are equal");
        assert_eq!(flag(&eq, N), 0);
        assert_eq!(flag(&eq, C), 1, "C set: 5 >=u 5");
        assert_eq!(flag(&eq, V), 0);

        // 5 - 7 is negative and borrows: N set, Z and C clear.
        let lt = exec_cmp(5, 7);
        assert_eq!(flag(&lt, Z), 0);
        assert_eq!(flag(&lt, N), 1, "N set: 5 - 7 is negative");
        assert_eq!(flag(&lt, C), 0, "C clear: 5 <u 7");

        // A b.eq reads Z: taken when set, fall-through (pc + 4) when clear.
        let run_beq = |z: u64| -> u64 {
            let mut ex = Executor::new(64);
            MachineContext::write_pc(&mut ex, 0x1000);
            MachineContext::write_register(&mut ex, "PSTATE", Z, APInt::new(1, z)).unwrap();
            let beq = arm64::BranchEqOpBuilder::new(&context)
                .attr("imm", AttributeValue::Int(4))
                .build();
            let mi = context
                .get_op(beq.id())
                .as_interface::<dyn MachineInstruction>()
                .expect("b.eq is a machine instruction");
            mi.execute(&mut ex).expect("b.eq executes");
            MachineContext::read_pc(&ex)
        };
        // imm=4, target = pc + (sext(imm) << 2) = 0x1000 + 16.
        assert_eq!(run_beq(1), 0x1010, "branch taken when Z is set");
        assert_eq!(run_beq(0), 0x1004, "fall-through (pc + width) when Z is clear");

        // `bl` writes two destinations: the link register (x30 = pc + 4) and PC.
        // Both used to be dropped because only one assignment was ever emitted.
        let mut ex = Executor::new(64);
        MachineContext::write_pc(&mut ex, 0x2000);
        let bl = arm64::BranchLinkOpBuilder::new(&context)
            .attr("imm", AttributeValue::Int(3))
            .build();
        let mi = context
            .get_op(bl.id())
            .as_interface::<dyn MachineInstruction>()
            .expect("bl is a machine instruction");
        mi.execute(&mut ex).expect("bl executes");
        let x30 = MachineContext::read_register(&ex, "GPR", 30).unwrap().to_u64();
        assert_eq!(x30, 0x2004, "link register holds the return address (pc + 4)");
        assert_eq!(
            MachineContext::read_pc(&ex),
            0x2000 + (3 << 2),
            "pc takes the branch target"
        );
    }
}
