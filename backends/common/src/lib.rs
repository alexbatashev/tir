use tir::helpers::dialect;

pub mod isel;
mod lexer;
mod operations;
mod parser;

pub use operations::*;

pub use lexer::Token;
pub use lexer::lex;
pub use parser::{AsmInstructionParser, AsmParser};
use tir::attributes::{AttributeValue, RegisterAttr};
use tir::sem_expr::{APInt, Expr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimTrap {
    MissingRegister {
        class: String,
        index: u16,
    },
    MissingAttribute {
        op: &'static str,
        attribute: &'static str,
    },
    InvalidAttribute {
        op: &'static str,
        attribute: &'static str,
    },
    InvalidInstruction {
        op: &'static str,
        reason: String,
    },
    BadAddress {
        address: u64,
        size: usize,
    },
    ProgramNotLoaded,
    PcNotMapped {
        pc: u64,
    },
    MaxCyclesExceeded {
        max_cycles: u64,
        until_pc: u64,
    },
}

pub trait MachineContext {
    fn read_register(&self, class: &str, index: u16) -> Result<APInt, SimTrap>;
    fn write_register(&mut self, class: &str, index: u16, value: APInt) -> Result<(), SimTrap>;
    fn read_memory(&self, address: u64, size: usize) -> Result<u64, SimTrap>;
    fn write_memory(&mut self, address: u64, size: usize, value: u64) -> Result<(), SimTrap>;
    fn read_pc(&self) -> u64;
    fn write_pc(&mut self, value: u64);
}

pub trait MachineInstruction {
    fn verify_interface(
        &self,
        _this: &dyn tir::Operation,
        _context: &tir::Context,
    ) -> Result<(), tir::Error> {
        Ok(())
    }
    fn mnemonic(&self) -> &'static str;
    fn width_bytes(&self) -> u8;
    fn execute(&self, machine: &mut dyn MachineContext) -> Result<(), SimTrap>;
    fn explicit_pc_write(&self) -> bool {
        false
    }
}

pub fn register_attr(
    attrs: &[tir::attributes::NamedAttribute],
    name: &str,
) -> Option<(String, u16)> {
    attrs.iter().find_map(|attr| {
        if attr.name != name {
            return None;
        }
        match &attr.value {
            AttributeValue::Register(RegisterAttr::Physical { class, index }) => {
                Some((class.clone(), *index))
            }
            _ => None,
        }
    })
}

pub fn int_attr(attrs: &[tir::attributes::NamedAttribute], name: &str) -> Option<i64> {
    attrs.iter().find_map(|attr| {
        if attr.name != name {
            return None;
        }
        match attr.value {
            AttributeValue::Int(i) => Some(i),
            AttributeValue::UInt(u) => i64::try_from(u).ok(),
            _ => None,
        }
    })
}

pub fn resolve_expr_symbols<F>(expr: &Expr, mut resolver: F) -> Result<Expr, SimTrap>
where
    F: FnMut(u32) -> Result<Option<APInt>, SimTrap>,
{
    fn go<F>(expr: &Expr, resolver: &mut F) -> Result<Expr, SimTrap>
    where
        F: FnMut(u32) -> Result<Option<APInt>, SimTrap>,
    {
        Ok(match expr {
            Expr::Symbol(sym) => {
                Expr::Int(resolver(*sym)?.ok_or_else(|| SimTrap::InvalidInstruction {
                    op: "unknown",
                    reason: format!("unbound symbol {}", sym),
                })?)
            }
            Expr::Int(i) => Expr::Int(i.clone()),
            Expr::Float(f) => Expr::Float(f.clone()),
            Expr::Bits(b) => Expr::Bits(b.clone()),
            Expr::Bool(b) => Expr::Bool(*b),
            Expr::If { cond, then, else_ } => Expr::If {
                cond: Box::new(go(cond, resolver)?),
                then: Box::new(go(then, resolver)?),
                else_: Box::new(go(else_, resolver)?),
            },
            Expr::Add(a, b) => Expr::Add(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?)),
            Expr::Sub(a, b) => Expr::Sub(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?)),
            Expr::Mul(a, b) => Expr::Mul(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?)),
            Expr::Div(a, b) => Expr::Div(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?)),
            Expr::UDiv(a, b) => Expr::UDiv(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?)),
            Expr::Eq(a, b) => Expr::Eq(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?)),
            Expr::Ne(a, b) => Expr::Ne(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?)),
            Expr::Lt(a, b) => Expr::Lt(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?)),
            Expr::Le(a, b) => Expr::Le(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?)),
            Expr::Gt(a, b) => Expr::Gt(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?)),
            Expr::Ge(a, b) => Expr::Ge(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?)),
            Expr::ULt(a, b) => Expr::ULt(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?)),
            Expr::ULe(a, b) => Expr::ULe(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?)),
            Expr::UGt(a, b) => Expr::UGt(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?)),
            Expr::UGe(a, b) => Expr::UGe(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?)),
            Expr::ShiftLeft(a, b) => {
                Expr::ShiftLeft(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?))
            }
            Expr::ShiftRightLogic(a, b) => {
                Expr::ShiftRightLogic(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?))
            }
            Expr::ShiftRightArithmetic(a, b) => {
                Expr::ShiftRightArithmetic(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?))
            }
            Expr::Or(a, b) => Expr::Or(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?)),
            Expr::And(a, b) => Expr::And(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?)),
            Expr::Xor(a, b) => Expr::Xor(Box::new(go(a, resolver)?), Box::new(go(b, resolver)?)),
            Expr::Clamp { input, min, max } => Expr::Clamp {
                input: Box::new(go(input, resolver)?),
                min: Box::new(go(min, resolver)?),
                max: Box::new(go(max, resolver)?),
            },
            Expr::Log2Ceil(input) => Expr::Log2Ceil(Box::new(go(input, resolver)?)),
            Expr::Extract { input, high, low } => Expr::Extract {
                input: Box::new(go(input, resolver)?),
                high: Box::new(go(high, resolver)?),
                low: Box::new(go(low, resolver)?),
            },
            Expr::ZExt { input, width } => Expr::ZExt {
                input: Box::new(go(input, resolver)?),
                width: Box::new(go(width, resolver)?),
            },
            Expr::SExt { input, width } => Expr::SExt {
                input: Box::new(go(input, resolver)?),
                width: Box::new(go(width, resolver)?),
            },
            Expr::Load {
                addr,
                bytes,
                signed,
            } => Expr::Load {
                addr: Box::new(go(addr, resolver)?),
                bytes: Box::new(go(bytes, resolver)?),
                signed: Box::new(go(signed, resolver)?),
            },
            Expr::Store { addr, bytes, value } => Expr::Store {
                addr: Box::new(go(addr, resolver)?),
                bytes: Box::new(go(bytes, resolver)?),
                value: Box::new(go(value, resolver)?),
            },
            Expr::Sqrt(a) => Expr::Sqrt(Box::new(go(a, resolver)?)),
            Expr::Fma { a, b, c } => Expr::Fma {
                a: Box::new(go(a, resolver)?),
                b: Box::new(go(b, resolver)?),
                c: Box::new(go(c, resolver)?),
            },
            Expr::IntToBits(a) => Expr::IntToBits(Box::new(go(a, resolver)?)),
            Expr::FloatToBits(a) => Expr::FloatToBits(Box::new(go(a, resolver)?)),
            Expr::BitsToInt {
                bits,
                width,
                signed,
            } => Expr::BitsToInt {
                bits: Box::new(go(bits, resolver)?),
                width: *width,
                signed: *signed,
            },
            Expr::BitsToFloat {
                bits,
                exp_width,
                mant_width,
                explicit_leading_bit,
            } => Expr::BitsToFloat {
                bits: Box::new(go(bits, resolver)?),
                exp_width: *exp_width,
                mant_width: *mant_width,
                explicit_leading_bit: *explicit_leading_bit,
            },
        })
    }

    go(expr, &mut resolver)
}

pub mod ops {
    pub use crate::operations::*;
}

dialect! {
    AsmDialect {
        name: "asm",
        operations: [SectionOp, SectionEndOp, SymbolOp, SymbolEndOp, BlockEndOp],
    }
}
