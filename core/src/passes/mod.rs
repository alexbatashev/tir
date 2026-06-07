//! Transformation passes over generic TIR interfaces.

pub mod instcombine;
pub mod mem2reg;

pub use instcombine::InstCombinePass;
pub use mem2reg::Mem2RegPass;
