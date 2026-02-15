use crate::OpId;
use crate::TypeId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ValueId(u32);

impl ValueId {
    pub(crate) fn new(id: u32) -> Self {
        Self(id)
    }

    pub fn number(&self) -> u32 {
        self.0
    }

    pub fn from_number(n: u32) -> Self {
        Self(n)
    }
}

#[derive(Debug, Clone)]
pub struct Value {
    id: ValueId,
    ty: TypeId,
    defining_op: Option<OpId>,
    uses: Vec<Use>,
}

impl Value {
    pub fn new(id: ValueId, ty: TypeId, defining_op: Option<OpId>) -> Self {
        Self {
            id,
            ty,
            defining_op,
            uses: vec![],
        }
    }

    pub fn id(&self) -> ValueId {
        self.id
    }

    pub fn ty(&self) -> TypeId {
        self.ty
    }

    pub fn defining_op(&self) -> Option<OpId> {
        self.defining_op
    }

    pub fn with_defining_op(mut self, op: OpId) -> Self {
        self.defining_op = Some(op);
        self
    }
}

#[derive(Debug, Clone)]
pub struct Use {
    op: OpId,
    index: usize,
}
