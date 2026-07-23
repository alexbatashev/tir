use crate::backend::liveness::PhysReg;
use crate::{Context, TypeId, ValueId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueKind {
    Int,
    Float,
    Vector,
}

pub fn type_kind(context: &Context, ty: TypeId) -> ValueKind {
    let data = context.get_type_data(ty);
    let data = data.as_ref() as &dyn std::any::Any;
    if data.downcast_ref::<crate::builtin::FloatType>().is_some() {
        ValueKind::Float
    } else if data.downcast_ref::<crate::vector::VectorType>().is_some() {
        ValueKind::Vector
    } else {
        ValueKind::Int
    }
}

pub fn value_kind(context: &Context, value: ValueId) -> ValueKind {
    type_kind(context, context.get_value(value).ty())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overflow {
    Chain(ValueKind),
    Stack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStyle {
    FrameSlots,
    PushPop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassifierKind {
    Riscv,
    Aapcs64,
    Sysv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StackLayout {
    pub align: u32,
    pub slot_size: u32,
    pub red_zone: u32,
    pub grows_down: bool,
    pub save_style: SaveStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PassSeq {
    pub kind: ValueKind,
    pub regs: &'static [PhysReg],
    pub overflow: Overflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbiInfo {
    pub name: &'static str,
    pub stack: StackLayout,
    pub sp: PhysReg,
    pub ra: Option<PhysReg>,
    pub fp: Option<PhysReg>,
    pub args: &'static [PassSeq],
    pub rets: &'static [PassSeq],
    pub callee_saved: &'static [PhysReg],
    pub caller_saved: &'static [PhysReg],
    pub reserved: &'static [PhysReg],
    pub classifier: ClassifierKind,
}
