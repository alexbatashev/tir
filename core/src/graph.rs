use crate::Context;

pub trait NodeKind {
    fn is_leaf(&self, ctx: &Context) -> bool;

    fn num_children(&self, ctx: &Context) -> usize;
}

pub trait Dag<NK: NodeKind, L> {}

pub struct PostOrderDag<NK: NodeKind, L> {}

// TODO do I even need this?!
pub struct PreOrderDag<NK: NodeKind, L> {}

impl<NK: NodeKind, L> Dag<NK, L> for PostOrderDag<NK, L> {}
