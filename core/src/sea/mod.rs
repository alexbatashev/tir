//! Sea: TIR's core (green) representation.
//!
//! One canonical, data-oriented region-hierarchical value+state graph, following
//! the RVSDG specification (Reissmann et al. 2020) as pinned in
//! `docs/design/sea.md`. [`graph`] holds the storage and its invariants,
//! [`kinds`] the canonical structural op types and their signature laws,
//! [`mutate`] the edit API, and [`raise`]/[`lower`] the translation to and from
//! the `scf` dialect. [`view`] is the red layer: the e-graph rendering of a
//! region's pure value terms, which [`commit`] lands back as one edit.

pub mod commit;
pub mod graph;
pub mod kinds;
pub mod lower;
pub mod mutate;
pub mod raise;
mod sym;
pub mod view;

#[cfg(test)]
mod tests;

pub use commit::commit;
pub use graph::{Graph, NodeId, OpTypeId, Origin, PortType, RegionId};
pub use lower::lower_function;
pub use raise::{Raised, raise_function};
pub use view::{View, ViewCache};

/// What went wrong building, verifying, or translating a graph. Messages name
/// the mismatch and the port it happened on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Error(message.into())
    }

    /// Prefix the message with where the failure was found.
    pub fn context(self, context: impl std::fmt::Display) -> Self {
        Error(format!("{context}: {}", self.0))
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<Error> for crate::Error {
    fn from(error: Error) -> Self {
        crate::Error::VerificationError(error.0)
    }
}
