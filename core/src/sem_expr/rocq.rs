use super::{APInt, Expr};
use std::io::Write;

/// Trait for resolving symbols to Rocq expressions.
pub trait SymbolResolver {
    /// Resolve a symbol ID to a Rocq expression string.
    fn resolve(&self, symbol_id: u32) -> Result<String, String>;
}

/// Emit a semantic expression as Rocq/Coq code.
pub fn emit<W: Write, R: SymbolResolver>(
    expr: &Expr,
    output: &mut W,
    resolver: &R,
) -> std::io::Result<()> {
    emit_expr(expr, output, resolver)
}

fn emit_expr<W: Write, R: SymbolResolver>(
    expr: &Expr,
    output: &mut W,
    resolver: &R,
) -> std::io::Result<()> {
    match expr {
        Expr::Int(int) => emit_int(int, output),
        Expr::Bool(b) => write!(output, "{}", if *b { "true" } else { "false" }),
        Expr::Symbol(id) => {
            let resolved = resolver
                .resolve(*id)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
            write!(output, "{}", resolved)
        }
        Expr::Bits(bits) => write!(
            output,
            "(tmdl_word_of_nat {} {})",
            bits.width(),
            bits.to_u128()
        ),

        Expr::Add(lhs, rhs) => emit_infix("+", lhs, rhs, output, resolver),
        Expr::Sub(lhs, rhs) => emit_infix("-", lhs, rhs, output, resolver),
        Expr::Mul(lhs, rhs) => emit_infix("*", lhs, rhs, output, resolver),
        Expr::Div(lhs, rhs) => emit_infix("/", lhs, rhs, output, resolver),
        Expr::UDiv(lhs, rhs) => emit_infix("/", lhs, rhs, output, resolver),
        Expr::Eq(lhs, rhs) => emit_infix("=", lhs, rhs, output, resolver),
        Expr::Ne(lhs, rhs) => emit_infix("<>", lhs, rhs, output, resolver),
        Expr::Lt(lhs, rhs) => emit_infix("<", lhs, rhs, output, resolver),
        Expr::Le(lhs, rhs) => emit_infix("<=", lhs, rhs, output, resolver),
        Expr::Gt(lhs, rhs) => emit_infix(">", lhs, rhs, output, resolver),
        Expr::Ge(lhs, rhs) => emit_infix(">=", lhs, rhs, output, resolver),
        Expr::ULt(lhs, rhs) => emit_infix("<", lhs, rhs, output, resolver),
        Expr::ULe(lhs, rhs) => emit_infix("<=", lhs, rhs, output, resolver),
        Expr::UGt(lhs, rhs) => emit_infix(">", lhs, rhs, output, resolver),
        Expr::UGe(lhs, rhs) => emit_infix(">=", lhs, rhs, output, resolver),
        Expr::And(lhs, rhs) => emit_infix("&&&", lhs, rhs, output, resolver),
        Expr::Or(lhs, rhs) => emit_infix("|||", lhs, rhs, output, resolver),
        Expr::Xor(lhs, rhs) => emit_infix("^^^", lhs, rhs, output, resolver),
        Expr::ShiftLeft(lhs, rhs) => emit_infix("<<<", lhs, rhs, output, resolver),
        Expr::ShiftRightLogic(lhs, rhs) | Expr::ShiftRightArithmetic(lhs, rhs) => {
            emit_infix(">>>", lhs, rhs, output, resolver)
        }

        Expr::If { cond, then, else_ } => {
            write!(output, "(if ")?;
            emit_expr(cond, output, resolver)?;
            write!(output, " then ")?;
            emit_expr(then, output, resolver)?;
            write!(output, " else ")?;
            emit_expr(else_, output, resolver)?;
            write!(output, ")")
        }

        // Keep this as a pass-through until the Rocq support library provides a shared helper.
        Expr::Extract { input, .. } => emit_expr(input, output, resolver),

        Expr::ZExt { input, .. } | Expr::SExt { input, .. } => emit_expr(input, output, resolver),

        Expr::Clamp { input, min, max } => {
            write!(output, "(let x := ")?;
            emit_expr(input, output, resolver)?;
            write!(output, " in if x <? ")?;
            emit_expr(min, output, resolver)?;
            write!(output, " then ")?;
            emit_expr(min, output, resolver)?;
            write!(output, " else if ")?;
            emit_expr(max, output, resolver)?;
            write!(output, " <? x then ")?;
            emit_expr(max, output, resolver)?;
            write!(output, " else x)")
        }

        Expr::IntToBits(inner) | Expr::FloatToBits(inner) | Expr::Sqrt(inner) => {
            emit_expr(inner, output, resolver)
        }
        Expr::BitsToInt { bits, .. } => emit_expr(bits, output, resolver),
        Expr::BitsToFloat { bits, .. } => emit_expr(bits, output, resolver),

        Expr::Float(_) | Expr::Fma { .. } => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Floating-point emission not yet supported for Rocq",
        )),
    }
}

fn emit_infix<W: Write, R: SymbolResolver>(
    op: &str,
    lhs: &Expr,
    rhs: &Expr,
    output: &mut W,
    resolver: &R,
) -> std::io::Result<()> {
    write!(output, "(")?;
    emit_expr(lhs, output, resolver)?;
    write!(output, " {} ", op)?;
    emit_expr(rhs, output, resolver)?;
    write!(output, ")")
}

fn emit_int<W: Write>(int: &APInt, output: &mut W) -> std::io::Result<()> {
    write!(output, "(tmdl_word_of_nat 64 {})", int.to_u64())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestResolver;

    impl SymbolResolver for TestResolver {
        fn resolve(&self, symbol_id: u32) -> Result<String, String> {
            Ok(format!("sym{}", symbol_id))
        }
    }

    #[test]
    fn test_emit_symbol() {
        let expr = Expr::Symbol(3);
        let mut out = Vec::new();
        emit(&expr, &mut out, &TestResolver).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "sym3");
    }

    #[test]
    fn test_emit_add() {
        let expr = Expr::Add(
            Box::new(Expr::Int(APInt::new(8, 1))),
            Box::new(Expr::Int(APInt::new(8, 2))),
        );
        let mut out = Vec::new();
        emit(&expr, &mut out, &TestResolver).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "((tmdl_word_of_nat 64 1) + (tmdl_word_of_nat 64 2))"
        );
    }
}
