use tir_be_common::SimTrap;

#[derive(Debug)]
pub enum Error {
    ProgramAlreadyLoaded,
    ProgramNotLoaded,
    EntrySymbolNotFound(String),
    MissingSymbolName,
    NoSymbolsFound,
    MissingFallthrough { pc: u64 },
    Trap(SimTrap),
}

impl From<SimTrap> for Error {
    fn from(value: SimTrap) -> Self {
        Self::Trap(value)
    }
}
