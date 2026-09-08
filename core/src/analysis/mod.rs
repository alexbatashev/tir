pub mod affine;
pub mod defuse;
pub mod escape_facts;
pub mod exits;
mod manager;
pub mod objects;
pub mod regions;
pub mod slots;
pub mod solver;

pub use affine::AffineView;
pub use defuse::{DefUse, OpRegs, PhysReg, execution_regs, op_regs};
pub use escape_facts::{Escape, EscapeFacts};
pub use manager::*;
pub use objects::Base;
