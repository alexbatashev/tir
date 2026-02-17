use std::collections::{BTreeMap, HashMap};

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

                if op.name == SectionOp::name() {
                    let section = op.clone().as_op::<SectionOp>().unwrap();
                    blocks_to_visit.push(section.body());
                    continue;
                }

                if op.name != SymbolOp::name() {
                    continue;
                }

                let symbol = op.clone().as_op::<SymbolOp>().unwrap();
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
    registers: HashMap<(String, u16), u64>,
    memory: Vec<u8>,
    pc: u64,
    pc_explicitly_written: bool,
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

    pub fn run(&mut self, until_pc: u64, max_cycles: u64) -> Result<(), Error> {
        for _cycle in 0..max_cycles {
            if self.pc == until_pc {
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
            for op_id in instructions {
                let op = context.get_op(op_id);
                let machine_inst = op
                    .clone()
                    .as_interface::<dyn MachineInstruction>()
                    .ok_or_else(|| SimTrap::InvalidInstruction {
                        op: op.name,
                        reason: "operation does not implement MachineInstruction".to_string(),
                    })?;
                machine_inst.execute(self)?;
            }

            if !self.pc_explicitly_written {
                self.pc = fallthrough_pc.ok_or(Error::MissingFallthrough { pc: self.pc })?;
            }
        }

        Err(SimTrap::MaxCyclesExceeded {
            max_cycles,
            until_pc,
        }
        .into())
    }
}

impl MachineContext for Executor {
    fn read_register(&self, class: &str, index: u16) -> Result<tir::sem_expr::APInt, SimTrap> {
        if class == "GPR" && index == 0 {
            return Ok(tir::sem_expr::APInt::new(64, 0));
        }
        let key = (class.to_string(), index);
        let value = *self.registers.get(&key).unwrap_or(&0);
        Ok(tir::sem_expr::APInt::new(64, value))
    }

    fn write_register(
        &mut self,
        class: &str,
        index: u16,
        value: tir::sem_expr::APInt,
    ) -> Result<(), SimTrap> {
        if class == "GPR" && index == 0 {
            return Ok(());
        }
        self.registers
            .insert((class.to_string(), index), value.to_u64());
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
    use tir::sem_expr::APInt;
    use tir_be_common::AsmDialect;
    use tir_riscv::RiscvDialect;

    use crate::{Executor, ProgramBuilder, error::Error};

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
}
