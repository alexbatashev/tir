// Adapted from isla-lib's BSD-2-Clause `simplify` SMT writer. Kept local
// because the upstream formatter is private while verifier traces must remain
// structured instead of being printed and reparsed.

use std::io::{Error, Write};

use isla_lib::bitvector::{write_bits64, BV};
use isla_lib::ir::{Loc, SharedState, Symtab};
use isla_lib::smt::{smtlib, Sym};
use isla_lib::zencode;

pub fn ty(ty: &smtlib::Ty, symtab: &Symtab) -> std::io::Result<String> {
    let mut out = Vec::new();
    write_ty(&mut out, ty, symtab)?;
    String::from_utf8(out).map_err(Error::other)
}

pub fn exp_sym<B: BV>(exp: &smtlib::Exp<Sym>, shared: &SharedState<B>) -> std::io::Result<String> {
    format_exp(exp, shared)
}

pub fn exp_loc<B: BV>(
    exp: &smtlib::Exp<Loc<String>>,
    shared: &SharedState<B>,
) -> std::io::Result<String> {
    format_exp(exp, shared)
}

fn format_exp<B: BV, V: WriteVar>(
    exp: &smtlib::Exp<V>,
    shared: &SharedState<B>,
) -> std::io::Result<String> {
    let mut out = Vec::new();
    write_exp(&mut out, exp, shared)?;
    String::from_utf8(out).map_err(Error::other)
}

fn write_ty(out: &mut dyn Write, ty: &smtlib::Ty, symtab: &Symtab) -> std::io::Result<()> {
    use smtlib::Ty::*;
    match ty {
        Bool => write!(out, "Bool"),
        BitVec(width) => write!(out, "(_ BitVec {width})"),
        Enum(id) => write!(out, "|{}|", zencode::decode(symtab.to_str(id.to_name()))),
        Array(domain, range) => {
            write!(out, "(Array ")?;
            write_ty(out, domain, symtab)?;
            write!(out, " ")?;
            write_ty(out, range, symtab)?;
            write!(out, ")")
        }
        Float(ebits, sbits) => write!(out, "(_ FloatingPoint {ebits} {sbits})"),
        RoundingMode => write!(out, "RoundingMode"),
    }
}

trait WriteVar {
    fn write_var(&self, out: &mut dyn Write) -> std::io::Result<()>;
}

impl WriteVar for Sym {
    fn write_var(&self, out: &mut dyn Write) -> std::io::Result<()> {
        write!(out, "v{self}")
    }
}

impl WriteVar for Loc<String> {
    fn write_var(&self, out: &mut dyn Write) -> std::io::Result<()> {
        match self {
            Loc::Id(name) => write!(out, "(|{}| nil)", zencode::decode(name)),
            _ => {
                write!(out, "(|{}| (", zencode::decode(&self.id()))?;
                let mut location = self;
                loop {
                    match location {
                        Loc::Id(_) => break,
                        Loc::Field(parent, field) => {
                            write!(out, "(_ field |{}|) ", zencode::decode(field))?;
                            location = parent;
                        }
                        Loc::Addr(parent) => location = parent,
                    }
                }
                write!(out, "))")
            }
        }
    }
}

fn unary<B: BV, V: WriteVar>(
    out: &mut dyn Write,
    name: &str,
    value: &smtlib::Exp<V>,
    shared: &SharedState<B>,
) -> std::io::Result<()> {
    write!(out, "({name} ")?;
    write_exp(out, value, shared)?;
    write!(out, ")")
}

fn binary<B: BV, V: WriteVar>(
    out: &mut dyn Write,
    name: &str,
    lhs: &smtlib::Exp<V>,
    rhs: &smtlib::Exp<V>,
    shared: &SharedState<B>,
) -> std::io::Result<()> {
    write!(out, "({name} ")?;
    write_exp(out, lhs, shared)?;
    write!(out, " ")?;
    write_exp(out, rhs, shared)?;
    write!(out, ")")
}

fn write_exp<B: BV, V: WriteVar>(
    out: &mut dyn Write,
    exp: &smtlib::Exp<V>,
    shared: &SharedState<B>,
) -> std::io::Result<()> {
    use smtlib::Exp::*;
    match exp {
        Var(value) => value.write_var(out),
        Bits(bits) => {
            write!(out, "#")?;
            if bits.len() % 4 == 0 {
                write!(out, "x")?;
                for index in (0..bits.len() / 4).rev() {
                    let base = index * 4;
                    let nibble = u8::from(bits[base])
                        | u8::from(bits[base + 1]) << 1
                        | u8::from(bits[base + 2]) << 2
                        | u8::from(bits[base + 3]) << 3;
                    write!(out, "{nibble:x}")?;
                }
                Ok(())
            } else {
                write!(out, "b")?;
                for bit in bits.iter().rev() {
                    write!(out, "{}", u8::from(*bit))?;
                }
                Ok(())
            }
        }
        Bits64(bits) => write_bits64(out, bits.lower_u64(), bits.len()),
        Enum(member) => {
            let members = shared
                .type_info
                .enums
                .get(&member.enum_id.to_name())
                .ok_or_else(|| Error::other("missing Isla enum"))?;
            write!(
                out,
                "|{}|",
                zencode::decode(shared.symtab.to_str(members[member.member]))
            )
        }
        Bool(value) => write!(out, "{value}"),
        Eq(lhs, rhs) => binary(out, "=", lhs, rhs, shared),
        Neq(lhs, rhs) => {
            write!(out, "(not ")?;
            binary(out, "=", lhs, rhs, shared)?;
            write!(out, ")")
        }
        And(lhs, rhs) => binary(out, "and", lhs, rhs, shared),
        Or(lhs, rhs) => binary(out, "or", lhs, rhs, shared),
        Not(value) => unary(out, "not", value, shared),
        Bvnot(value) => unary(out, "bvnot", value, shared),
        Bvand(lhs, rhs) => binary(out, "bvand", lhs, rhs, shared),
        Bvor(lhs, rhs) => binary(out, "bvor", lhs, rhs, shared),
        Bvxor(lhs, rhs) => binary(out, "bvxor", lhs, rhs, shared),
        Bvnand(lhs, rhs) => binary(out, "bvnand", lhs, rhs, shared),
        Bvnor(lhs, rhs) => binary(out, "bvnor", lhs, rhs, shared),
        Bvxnor(lhs, rhs) => binary(out, "bvxnor", lhs, rhs, shared),
        Bvneg(value) => unary(out, "bvneg", value, shared),
        Bvadd(lhs, rhs) => binary(out, "bvadd", lhs, rhs, shared),
        Bvsub(lhs, rhs) => binary(out, "bvsub", lhs, rhs, shared),
        Bvmul(lhs, rhs) => binary(out, "bvmul", lhs, rhs, shared),
        Bvudiv(lhs, rhs) => binary(out, "bvudiv", lhs, rhs, shared),
        Bvsdiv(lhs, rhs) => binary(out, "bvsdiv", lhs, rhs, shared),
        Bvurem(lhs, rhs) => binary(out, "bvurem", lhs, rhs, shared),
        Bvsrem(lhs, rhs) => binary(out, "bvsrem", lhs, rhs, shared),
        Bvsmod(lhs, rhs) => binary(out, "bvsmod", lhs, rhs, shared),
        Bvult(lhs, rhs) => binary(out, "bvult", lhs, rhs, shared),
        Bvslt(lhs, rhs) => binary(out, "bvslt", lhs, rhs, shared),
        Bvule(lhs, rhs) => binary(out, "bvule", lhs, rhs, shared),
        Bvsle(lhs, rhs) => binary(out, "bvsle", lhs, rhs, shared),
        Bvuge(lhs, rhs) => binary(out, "bvuge", lhs, rhs, shared),
        Bvsge(lhs, rhs) => binary(out, "bvsge", lhs, rhs, shared),
        Bvugt(lhs, rhs) => binary(out, "bvugt", lhs, rhs, shared),
        Bvsgt(lhs, rhs) => binary(out, "bvsgt", lhs, rhs, shared),
        Extract(high, low, value) => {
            write!(out, "((_ extract {high} {low}) ")?;
            write_exp(out, value, shared)?;
            write!(out, ")")
        }
        ZeroExtend(amount, value) => {
            write!(out, "((_ zero_extend {amount}) ")?;
            write_exp(out, value, shared)?;
            write!(out, ")")
        }
        SignExtend(amount, value) => {
            write!(out, "((_ sign_extend {amount}) ")?;
            write_exp(out, value, shared)?;
            write!(out, ")")
        }
        Bvshl(lhs, rhs) => binary(out, "bvshl", lhs, rhs, shared),
        Bvlshr(lhs, rhs) => binary(out, "bvlshr", lhs, rhs, shared),
        Bvashr(lhs, rhs) => binary(out, "bvashr", lhs, rhs, shared),
        Concat(lhs, rhs) => binary(out, "concat", lhs, rhs, shared),
        Ite(cond, then_value, else_value) => {
            write!(out, "(ite ")?;
            write_exp(out, cond, shared)?;
            write!(out, " ")?;
            write_exp(out, then_value, shared)?;
            write!(out, " ")?;
            write_exp(out, else_value, shared)?;
            write!(out, ")")
        }
        App(function, args) => {
            write!(out, "(v{function}")?;
            for arg in args {
                write!(out, " ")?;
                write_exp(out, arg, shared)?;
            }
            write!(out, ")")
        }
        Select(array, index) => binary(out, "select", array, index, shared),
        Store(array, index, value) => {
            write!(out, "(store ")?;
            write_exp(out, array, shared)?;
            write!(out, " ")?;
            write_exp(out, index, shared)?;
            write!(out, " ")?;
            write_exp(out, value, shared)?;
            write!(out, ")")
        }
        Distinct(values) => {
            write!(out, "(distinct")?;
            for value in values {
                write!(out, " ")?;
                write_exp(out, value, shared)?;
            }
            write!(out, ")")
        }
        FPConstant(value, ebits, sbits) => {
            use smtlib::FPConstant::*;
            let name = match value {
                NaN => "NaN",
                Inf { negative: false } => "+oo",
                Inf { negative: true } => "-oo",
                Zero { negative: false } => "+zero",
                Zero { negative: true } => "-zero",
            };
            write!(out, "(_ {name} {ebits} {sbits})")
        }
        FPRoundingMode(mode) => {
            use smtlib::FPRoundingMode::*;
            write!(
                out,
                "{}",
                match mode {
                    RoundNearestTiesToEven => "roundNearestTiesToEven",
                    RoundNearestTiesToAway => "roundNearestTiesToAway",
                    RoundTowardPositive => "roundTowardPositive",
                    RoundTowardNegative => "roundTowardNegative",
                    RoundTowardZero => "roundTowardZero",
                }
            )
        }
        FPUnary(op, value) => {
            use smtlib::FPUnary::*;
            let name = match op {
                Abs => "fp.abs".to_string(),
                Neg => "fp.neg".to_string(),
                IsNormal => "fp.isNormal".to_string(),
                IsSubnormal => "fp.isSubnormal".to_string(),
                IsZero => "fp.isZero".to_string(),
                IsInfinite => "fp.isInfinite".to_string(),
                IsNaN => "fp.isNaN".to_string(),
                IsNegative => "fp.isNegative".to_string(),
                IsPositive => "fp.isPositive".to_string(),
                FromIEEE(ebits, sbits) => format!("(_ to_fp {ebits} {sbits})"),
            };
            unary(out, &name, value, shared)
        }
        FPRoundingUnary(op, mode, value) => {
            use smtlib::FPRoundingUnary::*;
            let name = match op {
                Sqrt => "fp.sqrt".to_string(),
                RoundToIntegral => "fp.roundToIntegral".to_string(),
                Convert(ebits, sbits) | FromSigned(ebits, sbits) => {
                    format!("(_ to_fp {ebits} {sbits})")
                }
                FromUnsigned(ebits, sbits) => format!("(_ to_fp_unsigned {ebits} {sbits})"),
                ToSigned(width) => format!("(_ fp.to_sbv {width})"),
                ToUnsigned(width) => format!("(_ fp.to_ubv {width})"),
            };
            write!(out, "({name} ")?;
            write_exp(out, mode, shared)?;
            write!(out, " ")?;
            write_exp(out, value, shared)?;
            write!(out, ")")
        }
        FPBinary(op, lhs, rhs) => {
            use smtlib::FPBinary::*;
            binary(
                out,
                match op {
                    Rem => "fp.rem",
                    Min => "fp.min",
                    Max => "fp.max",
                    Leq => "fp.leq",
                    Lt => "fp.lt",
                    Geq => "fp.geq",
                    Gt => "fp.gt",
                    Eq => "fp.eq",
                },
                lhs,
                rhs,
                shared,
            )
        }
        FPRoundingBinary(op, mode, lhs, rhs) => {
            use smtlib::FPRoundingBinary::*;
            let name = match op {
                Add => "fp.add",
                Sub => "fp.sub",
                Mul => "fp.mul",
                Div => "fp.div",
            };
            write!(out, "({name} ")?;
            write_exp(out, mode, shared)?;
            write!(out, " ")?;
            write_exp(out, lhs, shared)?;
            write!(out, " ")?;
            write_exp(out, rhs, shared)?;
            write!(out, ")")
        }
        FPfma(mode, x, y, z) => {
            write!(out, "(fp.fma ")?;
            write_exp(out, mode, shared)?;
            write!(out, " ")?;
            write_exp(out, x, shared)?;
            write!(out, " ")?;
            write_exp(out, y, shared)?;
            write!(out, " ")?;
            write_exp(out, z, shared)?;
            write!(out, ")")
        }
    }
}
