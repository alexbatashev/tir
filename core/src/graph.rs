//! The generic graph machinery lives in the `tir-graph` crate; it is re-exported
//! here as `tir::graph::*`. On top of it this module layers the core-specific
//! per-node annotation — the originating [`OpId`] and result [`TypeId`] a DAG
//! node may carry — as the [`Dag::Annotation`] type [`NodeMeta`], with the
//! [`MetaDag`] / [`MetaMutDag`] accessors that read and write its two fields
//! independently.

pub use tir_graph::*;

use crate::{OpId, TypeId};

/// Provenance a [`Dag`] node may carry: the IR operation it came from and the
/// concrete result type it had there. Kept out of the node label and leaf payload
/// so it never affects e-node identity. Both fields are optional and set
/// independently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeMeta {
    pub original_op: Option<OpId>,
    pub actual_type: Option<TypeId>,
}

/// Reads the two [`NodeMeta`] fields off any [`Dag`] annotated with it.
pub trait MetaDag: Dag<Annotation = NodeMeta> {
    fn get_original_op(&self, id: NodeId) -> Option<OpId> {
        self.get_annotation(id).and_then(|m| m.original_op)
    }

    fn get_actual_type(&self, id: NodeId) -> Option<TypeId> {
        self.get_annotation(id).and_then(|m| m.actual_type)
    }
}

impl<D: Dag<Annotation = NodeMeta>> MetaDag for D {}

/// Sets either [`NodeMeta`] field on a [`MutDag`] without disturbing the other.
pub trait MetaMutDag: MutDag<Annotation = NodeMeta> {
    fn set_original_op(&mut self, id: NodeId, op: OpId) {
        let mut meta = self.get_annotation(id).copied().unwrap_or_default();
        meta.original_op = Some(op);
        self.set_annotation(id, meta);
    }

    fn set_actual_type(&mut self, id: NodeId, ty: TypeId) {
        let mut meta = self.get_annotation(id).copied().unwrap_or_default();
        meta.actual_type = Some(ty);
        self.set_annotation(id, meta);
    }
}

impl<D: MutDag<Annotation = NodeMeta>> MetaMutDag for D {}
