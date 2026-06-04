use crate::ValueId;

#[derive(Clone, Copy, Debug, Default)]
pub struct Operand(Option<ValueId>);

impl Operand {
    pub fn none() -> Self {
        Self(None)
    }

    pub fn some(value: ValueId) -> Self {
        Self(Some(value))
    }

    pub fn into_option(self) -> Option<ValueId> {
        self.0
    }
}

impl From<ValueId> for Operand {
    fn from(value: ValueId) -> Self {
        Self(Some(value))
    }
}

impl From<Option<ValueId>> for Operand {
    fn from(value: Option<ValueId>) -> Self {
        Self(value)
    }
}
