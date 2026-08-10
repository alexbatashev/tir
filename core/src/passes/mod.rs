//! Transformation passes over generic TIR interfaces.

mod cfg_cleanup;
pub mod dce;
pub mod instcombine;
pub mod lower_memory_intrinsics;
pub mod mem2reg;
pub mod scf_to_cfg;
pub mod sea_roundtrip;
pub mod symbol_uniqueness;

pub use dce::DeadCodeEliminationPass;
pub use instcombine::InstCombinePass;
pub use lower_memory_intrinsics::LowerMemoryIntrinsicsPass;
pub use mem2reg::Mem2RegPass;
pub use scf_to_cfg::ScfToCfgPass;
pub use sea_roundtrip::SeaRoundTripPass;
pub use symbol_uniqueness::CheckUniqueSymbolsPass;
