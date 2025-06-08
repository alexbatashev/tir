use crate::OpId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ValueId(u32);

#[derive(Debug, Clone)]
pub struct Value {
    id: ValueId,
    defining_op: Option<OpId>,
    uses: Vec<Use>,
}

#[derive(Debug, Clone)]
pub struct Use {
    op: OpId,
    index: usize,
}
