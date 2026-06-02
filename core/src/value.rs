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

    /// The operands that reference this value, as `(op, operand-index)` pairs.
    ///
    /// Maintained by the [`Context`](crate::Context): an entry is added when an
    /// operation is added to the context and removed when it is erased or replaced.
    /// Only `operands` are tracked — register operands that instruction selection
    /// stores in attributes (`RegisterAttr::Virtual`) are *not* reflected here.
    pub fn uses(&self) -> &[Use] {
        &self.uses
    }

    /// Whether any operand references this value. See [`Value::uses`].
    pub fn is_used(&self) -> bool {
        !self.uses.is_empty()
    }

    pub(crate) fn add_use(&mut self, op: OpId, index: usize) {
        self.uses.push(Use { op, index });
    }

    /// Drop every use contributed by `op` (an op may use a value at several indices).
    pub(crate) fn remove_uses_of(&mut self, op: OpId) {
        self.uses.retain(|u| u.op != op);
    }
}

/// A reference to a value from an operation's operand list: the using `op` and the
/// operand `index` within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Use {
    op: OpId,
    index: usize,
}

impl Use {
    pub fn op(&self) -> OpId {
        self.op
    }

    pub fn operand_index(&self) -> usize {
        self.index
    }
}
