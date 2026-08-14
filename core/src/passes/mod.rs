//! Transformation passes over generic TIR interfaces.

mod cfg_cleanup;
pub mod dce;
pub mod erase_state;
pub mod instcombine;
pub mod lower_memory_intrinsics;
pub mod restructure;
pub mod scf_to_cfg;
pub mod symbol_uniqueness;
pub mod thread_state;

pub use dce::DeadCodeEliminationPass;
pub use erase_state::EraseStatePass;
pub use instcombine::InstCombinePass;
pub use lower_memory_intrinsics::LowerMemoryIntrinsicsPass;
pub use restructure::RestructurePass;
pub use scf_to_cfg::ScfToCfgPass;
pub use symbol_uniqueness::CheckUniqueSymbolsPass;
pub use thread_state::ThreadStatePass;
