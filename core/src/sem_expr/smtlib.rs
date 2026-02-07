use super::{APInt, Expr};
use std::io::Write;

/// Trait for resolving symbols to SMT-LIB expressions.
pub trait SymbolResolver {
    /// Resolve a symbol ID to an SMT-LIB expression string.
    fn resolve(&self, symbol_id: u32) -> Result<String, String>;
}

/// Emit a semantic expression as SMT-LIB code.
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
        Expr::Bits(bits) => write!(output, "(_ bv{} {})", bits.to_u128(), bits.width()),

        Expr::Add(lhs, rhs) => emit_call2("bvadd", lhs, rhs, output, resolver),
        Expr::Sub(lhs, rhs) => emit_call2("bvsub", lhs, rhs, output, resolver),
        Expr::Mul(lhs, rhs) => emit_call2("bvmul", lhs, rhs, output, resolver),
        Expr::Div(lhs, rhs) => emit_call2("bvudiv", lhs, rhs, output, resolver),
        Expr::And(lhs, rhs) => emit_call2("bvand", lhs, rhs, output, resolver),
        Expr::Or(lhs, rhs) => emit_call2("bvor", lhs, rhs, output, resolver),
        Expr::Xor(lhs, rhs) => emit_call2("bvxor", lhs, rhs, output, resolver),
        Expr::ShiftLeft(lhs, rhs) => emit_call2("bvshl", lhs, rhs, output, resolver),
        Expr::ShiftRightLogic(lhs, rhs) => emit_call2("bvlshr", lhs, rhs, output, resolver),
        Expr::ShiftRightArithmetic(lhs, rhs) => emit_call2("bvashr", lhs, rhs, output, resolver),

        Expr::If { cond, then, else_ } => {
            write!(output, "(ite (not (= ")?;
            emit_expr(cond, output, resolver)?;
            write!(output, " (_ bv0 64))) ")?;
            emit_expr(then, output, resolver)?;
            write!(output, " ")?;
            emit_expr(else_, output, resolver)?;
            write!(output, ")")
        }

        Expr::Extract { input, high, low } => {
            let hi = extract_const_u32(high)?;
            let lo = extract_const_u32(low)?;
            write!(output, "((_ extract {} {}) ", hi, lo)?;
            emit_expr(input, output, resolver)?;
            write!(output, ")")
        }

        Expr::ZExt { input, width } => emit_extend(false, input, *width, output, resolver),
        Expr::SExt { input, width } => emit_extend(true, input, *width, output, resolver),

        Expr::Clamp { input, min, max } => {
            write!(output, "(let ((x ")?;
            emit_expr(input, output, resolver)?;
            write!(output, ")) (ite (bvult x ")?;
            emit_expr(min, output, resolver)?;
            write!(output, ") ")?;
            emit_expr(min, output, resolver)?;
            write!(output, " (ite (bvult ")?;
            emit_expr(max, output, resolver)?;
            write!(output, " x) ")?;
            emit_expr(max, output, resolver)?;
            write!(output, " x)))")
        }

        Expr::IntToBits(inner) | Expr::FloatToBits(inner) | Expr::Sqrt(inner) => {
            emit_expr(inner, output, resolver)
        }
        Expr::BitsToInt { bits, .. } => emit_expr(bits, output, resolver),
        Expr::BitsToFloat { bits, .. } => emit_expr(bits, output, resolver),

        Expr::Float(_) | Expr::Fma { .. } => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Floating-point emission not yet supported for SMT-LIB",
        )),
    }
}

fn emit_int<W: Write>(int: &APInt, output: &mut W) -> std::io::Result<()> {
    write!(output, "(_ bv{} 64)", int.to_u64())
}

fn emit_call2<W: Write, R: SymbolResolver>(
    op: &str,
    lhs: &Expr,
    rhs: &Expr,
    output: &mut W,
    resolver: &R,
) -> std::io::Result<()> {
    write!(output, "({} ", op)?;
    emit_expr(lhs, output, resolver)?;
    write!(output, " ")?;
    emit_expr(rhs, output, resolver)?;
    write!(output, ")")
}

fn emit_extend<W: Write, R: SymbolResolver>(
    signed: bool,
    input: &Expr,
    target_width: u32,
    output: &mut W,
    resolver: &R,
) -> std::io::Result<()> {
    let input_width = infer_width(input).unwrap_or(64);
    if target_width <= input_width {
        write!(output, "((_ extract {} 0) ", target_width - 1)?;
        emit_expr(input, output, resolver)?;
        write!(output, ")")
    } else {
        let ext = target_width - input_width;
        let op = if signed { "sign_extend" } else { "zero_extend" };
        write!(output, "((_ {} {}) ", op, ext)?;
        emit_expr(input, output, resolver)?;
        write!(output, ")")
    }
}

fn infer_width(expr: &Expr) -> Option<u32> {
    match expr {
        Expr::Int(int) => Some(int.width()),
        Expr::Bits(bits) => Some(bits.width() as u32),
        Expr::Bool(_) => Some(1),
        Expr::Symbol(_) => Some(64),
        Expr::Add(lhs, _)
        | Expr::Sub(lhs, _)
        | Expr::Mul(lhs, _)
        | Expr::Div(lhs, _)
        | Expr::And(lhs, _)
        | Expr::Or(lhs, _)
        | Expr::Xor(lhs, _)
        | Expr::ShiftLeft(lhs, _)
        | Expr::ShiftRightLogic(lhs, _)
        | Expr::ShiftRightArithmetic(lhs, _)
        | Expr::IntToBits(lhs)
        | Expr::FloatToBits(lhs)
        | Expr::Sqrt(lhs) => infer_width(lhs),
        Expr::If { then, .. } => infer_width(then),
        Expr::Extract { high, low, .. } => {
            let hi = extract_const_u32(high).ok()?;
            let lo = extract_const_u32(low).ok()?;
            Some(hi - lo + 1)
        }
        Expr::ZExt { width, .. } | Expr::SExt { width, .. } => Some(*width),
        Expr::Clamp { input, .. } => infer_width(input),
        Expr::BitsToInt { width, .. } => Some(*width),
        Expr::BitsToFloat {
            exp_width,
            mant_width,
            explicit_leading_bit,
            ..
        } => Some(exp_width + mant_width + if *explicit_leading_bit { 1 } else { 0 }),
        Expr::Float(_) | Expr::Fma { .. } => None,
    }
}

fn extract_const_u32(expr: &Expr) -> std::io::Result<u32> {
    match expr {
        Expr::Int(int) => Ok(int.to_u64() as u32),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "SMT-LIB extract bounds must be constants",
        )),
    }
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
        let expr = Expr::Symbol(7);
        let mut out = Vec::new();
        emit(&expr, &mut out, &TestResolver).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "sym7");
    }

    #[test]
    fn test_emit_add() {
        let expr = Expr::Add(
            Box::new(Expr::Int(APInt::new(8, 5))),
            Box::new(Expr::Int(APInt::new(8, 9))),
        );
        let mut out = Vec::new();
        emit(&expr, &mut out, &TestResolver).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "(bvadd (_ bv5 64) (_ bv9 64))"
        );
    }
}
