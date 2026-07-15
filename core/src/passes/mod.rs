//! Transformation passes over generic TIR interfaces.

pub mod dce;
pub mod instcombine;
pub mod mem2reg;
pub mod scf_to_cfg;

pub use dce::DeadCodeEliminationPass;
pub use instcombine::InstCombinePass;
pub use mem2reg::Mem2RegPass;
pub use scf_to_cfg::ScfToCfgPass;
