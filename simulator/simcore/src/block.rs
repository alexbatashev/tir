use tir::{BlockId, OpId};

#[derive(Debug, Clone)]
pub struct MachineBlock {
    pub block: BlockId,
    pub instructions: Vec<OpId>,
    pub start_address: u64,
    pub byte_len: u64,
    pub fallthrough_pc: Option<u64>,
}
