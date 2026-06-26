use crate::egraph::enode::ENode;

use super::Id;

pub struct EClass<N: ENode> {
    nodes: Vec<N>,
    parents: Vec<(N, Id)>,
}
