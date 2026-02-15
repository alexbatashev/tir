use crate::{MachineBlock, error::Error, utils::Num};

pub struct Program {
    blocks: Vec<MachineBlock>,
}

pub trait Executor {
    fn load(&mut self, program: Program) -> Result<(), Error>;
    fn step(&mut self) -> Result<(), Error>;
    /// Returns truncated or zero-extended value from register `reg_id` of register file `class`.
    fn read_register<T: Num>(&mut self, class: &str, reg_id: usize) -> Result<T, Error>;
    /// Saves truncated or zero-extended value into a register.
    fn write_register<T: Num>(&mut self, class: &str, reg_id: usize, value: T)
    -> Result<(), Error>;
    fn read_memory<T: Num>(&mut self, address: usize) -> Result<T, Error>;
    fn write_memory<T: Num>(&mut self, address: usize, value: T) -> Result<(), Error>;
}
