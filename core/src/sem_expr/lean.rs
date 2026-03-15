use super::{APInt, BitVec, Expr};
use std::io::Write;

/// Trait for resolving symbols to Lean expressions
pub trait SymbolResolver {
    /// Resolve a symbol ID to a Lean expression string
    fn resolve(&self, symbol_id: u32) -> Result<String, String>;
}

/// Emit a semantic expression as Lean 4 code using BitVec for integer computations
///
/// The emitted code uses Lean 4's BitVec type for all integer operations.
/// Symbols are resolved using the provided resolver.
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
        Expr::Bits(bitvec) => emit_bitvec(bitvec, output),
        Expr::Symbol(id) => {
            let resolved = resolver
                .resolve(*id)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
            write!(output, "{}", resolved)
        }

        // Binary operations
        Expr::Add(lhs, rhs) => emit_binary_op("BitVec.add", lhs, rhs, output, resolver),
        Expr::Sub(lhs, rhs) => emit_binary_op("BitVec.sub", lhs, rhs, output, resolver),
        Expr::Mul(lhs, rhs) => emit_mul(lhs, rhs, output, resolver),
        Expr::Eq(lhs, rhs) => emit_binary_op("Eq", lhs, rhs, output, resolver),
        Expr::Ne(lhs, rhs) => {
            write!(output, "(Not (Eq ")?;
            emit_expr(lhs, output, resolver)?;
            write!(output, " ")?;
            emit_expr(rhs, output, resolver)?;
            write!(output, "))")
        }
        Expr::Lt(lhs, rhs) => emit_binary_op("BitVec.slt", lhs, rhs, output, resolver),
        Expr::Le(lhs, rhs) => emit_binary_op("BitVec.sle", lhs, rhs, output, resolver),
        Expr::Gt(lhs, rhs) => emit_binary_op("BitVec.sgt", lhs, rhs, output, resolver),
        Expr::Ge(lhs, rhs) => emit_binary_op("BitVec.sge", lhs, rhs, output, resolver),
        Expr::ULt(lhs, rhs) => emit_binary_op("BitVec.ult", lhs, rhs, output, resolver),
        Expr::ULe(lhs, rhs) => emit_binary_op("BitVec.ule", lhs, rhs, output, resolver),
        Expr::UGt(lhs, rhs) => emit_binary_op("BitVec.ugt", lhs, rhs, output, resolver),
        Expr::UGe(lhs, rhs) => emit_binary_op("BitVec.uge", lhs, rhs, output, resolver),
        Expr::And(lhs, rhs) => emit_binary_op("BitVec.and", lhs, rhs, output, resolver),
        Expr::Or(lhs, rhs) => emit_binary_op("BitVec.or", lhs, rhs, output, resolver),
        Expr::Xor(lhs, rhs) => emit_binary_op("BitVec.xor", lhs, rhs, output, resolver),

        // Shift operations
        Expr::ShiftLeft(lhs, rhs) => emit_shift_left(lhs, rhs, output, resolver),
        Expr::ShiftRightLogic(lhs, rhs) => emit_shift_right_logic(lhs, rhs, output, resolver),
        Expr::ShiftRightArithmetic(lhs, rhs) => emit_shift_right_arith(lhs, rhs, output, resolver),

        // Division
        Expr::Div(lhs, rhs) => emit_binary_op("BitVec.sdiv", lhs, rhs, output, resolver),
        Expr::UDiv(lhs, rhs) => emit_binary_op("BitVec.udiv", lhs, rhs, output, resolver),

        // Conditional
        Expr::If { cond, then, else_ } => {
            write!(output, "if ")?;
            emit_expr(cond, output, resolver)?;
            write!(output, " then ")?;
            emit_expr(then, output, resolver)?;
            write!(output, " else ")?;
            emit_expr(else_, output, resolver)
        }

        // Extract operation
        Expr::Extract { input, high, low } => {
            write!(output, "(BitVec.extractLsb ")?;
            emit_expr(low, output, resolver)?;
            write!(output, " ")?;
            emit_expr(high, output, resolver)?;
            write!(output, " ")?;
            emit_expr(input, output, resolver)?;
            write!(output, ")")
        }

        // Extension operations
        Expr::ZExt { input, width } => {
            let width = extract_const_u32(width)?;
            write!(output, "(BitVec.zeroExtend {} ", width)?;
            emit_expr(input, output, resolver)?;
            write!(output, ")")
        }

        Expr::SExt { input, width } => {
            let width = extract_const_u32(width)?;
            write!(output, "(BitVec.signExtend {} ", width)?;
            emit_expr(input, output, resolver)?;
            write!(output, ")")
        }

        // Clamp operation
        Expr::Clamp { input, min, max } => {
            write!(output, "(let x := ")?;
            emit_expr(input, output, resolver)?;
            write!(output, "; if x.ult ")?;
            emit_expr(min, output, resolver)?;
            write!(output, " then ")?;
            emit_expr(min, output, resolver)?;
            write!(output, " else if ")?;
            emit_expr(max, output, resolver)?;
            write!(output, ".ult x then ")?;
            emit_expr(max, output, resolver)?;
            write!(output, " else x)")
        }

        Expr::Log2Ceil(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "log2Ceil is not yet supported in Lean emission",
        )),

        // Conversion operations
        Expr::IntToBits(int_expr) => {
            // IntToBits is a no-op in Lean since Int is already BitVec
            emit_expr(int_expr, output, resolver)
        }
        Expr::BitsToInt { bits, .. } => {
            // BitsToInt is a no-op in Lean since BitVec is the representation
            emit_expr(bits, output, resolver)
        }

        Expr::Load { .. } | Expr::Store { .. } => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Memory operations are not yet supported in Lean emission",
        )),

        // Unsupported operations for now
        Expr::Float(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Float not yet supported in Lean emission",
        )),
        Expr::Sqrt(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Sqrt not yet supported in Lean emission",
        )),
        Expr::Fma { .. } => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Fma not yet supported in Lean emission",
        )),
        Expr::FloatToBits(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "FloatToBits not yet supported in Lean emission",
        )),
        Expr::BitsToFloat { .. } => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "BitsToFloat not yet supported in Lean emission",
        )),
    }
}

fn emit_int<W: Write>(int: &APInt, output: &mut W) -> std::io::Result<()> {
    // Emit as BitVec literal: (BitVec.ofNat width value)
    write!(output, "(BitVec.ofNat {} {})", int.width(), int.to_u64())
}

fn emit_bitvec<W: Write>(bitvec: &BitVec, output: &mut W) -> std::io::Result<()> {
    // For BitVecs <= 128 bits, emit as BitVec.ofNat
    if bitvec.width() <= 128 {
        write!(
            output,
            "(BitVec.ofNat {} {})",
            bitvec.width(),
            bitvec.to_u128()
        )
    } else {
        // For larger BitVecs, we need to emit as a byte array or hex literal
        // For now, emit as a hex literal using the bytes representation
        let bytes = bitvec.to_bytes();
        write!(output, "(BitVec.ofNat {} 0x", bitvec.width())?;
        for byte in bytes.iter().rev() {
            write!(output, "{:02x}", byte)?;
        }
        write!(output, ")")
    }
}

fn emit_binary_op<W: Write, R: SymbolResolver>(
    op_name: &str,
    lhs: &Expr,
    rhs: &Expr,
    output: &mut W,
    resolver: &R,
) -> std::io::Result<()> {
    write!(output, "({} ", op_name)?;
    emit_expr(lhs, output, resolver)?;
    write!(output, " ")?;
    emit_expr(rhs, output, resolver)?;
    write!(output, ")")
}

fn emit_mul<W: Write, R: SymbolResolver>(
    lhs: &Expr,
    rhs: &Expr,
    output: &mut W,
    resolver: &R,
) -> std::io::Result<()> {
    // Use tir_utils.mul which handles N+M width multiplication correctly
    emit_binary_op("tir_utils.mul", lhs, rhs, output, resolver)
}

fn extract_const_u32(expr: &Expr) -> std::io::Result<u32> {
    match expr {
        Expr::Int(int) => Ok(int.to_u64() as u32),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Lean extension width must be a constant Int",
        )),
    }
}

fn emit_shift_left<W: Write, R: SymbolResolver>(
    lhs: &Expr,
    rhs: &Expr,
    output: &mut W,
    resolver: &R,
) -> std::io::Result<()> {
    write!(output, "(BitVec.shiftLeft ")?;
    emit_expr(lhs, output, resolver)?;
    write!(output, " ")?;
    emit_expr(rhs, output, resolver)?;
    write!(output, ")")
}

fn emit_shift_right_logic<W: Write, R: SymbolResolver>(
    lhs: &Expr,
    rhs: &Expr,
    output: &mut W,
    resolver: &R,
) -> std::io::Result<()> {
    write!(output, "(BitVec.ushiftRight ")?;
    emit_expr(lhs, output, resolver)?;
    write!(output, " ")?;
    emit_expr(rhs, output, resolver)?;
    write!(output, ")")
}

fn emit_shift_right_arith<W: Write, R: SymbolResolver>(
    lhs: &Expr,
    rhs: &Expr,
    output: &mut W,
    resolver: &R,
) -> std::io::Result<()> {
    write!(output, "(BitVec.sshiftRight ")?;
    emit_expr(lhs, output, resolver)?;
    write!(output, " ")?;
    emit_expr(rhs, output, resolver)?;
    write!(output, ")")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestResolver;

    impl SymbolResolver for TestResolver {
        fn resolve(&self, symbol_id: u32) -> Result<String, String> {
            Ok(format!("reg{}", symbol_id))
        }
    }

    #[test]
    fn test_emit_int_literal() {
        let expr = Expr::Int(APInt::new(8, 42));
        let mut output = Vec::new();
        let resolver = TestResolver;

        emit(&expr, &mut output, &resolver).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "(BitVec.ofNat 8 42)");
    }

    #[test]
    fn test_emit_bool() {
        let expr = Expr::Bool(true);
        let mut output = Vec::new();
        let resolver = TestResolver;

        emit(&expr, &mut output, &resolver).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "true");
    }

    #[test]
    fn test_emit_symbol() {
        let expr = Expr::Symbol(5);
        let mut output = Vec::new();
        let resolver = TestResolver;

        emit(&expr, &mut output, &resolver).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "reg5");
    }

    #[test]
    fn test_emit_add() {
        let expr = Expr::Add(
            Box::new(Expr::Int(APInt::new(8, 10))),
            Box::new(Expr::Int(APInt::new(8, 20))),
        );
        let mut output = Vec::new();
        let resolver = TestResolver;

        emit(&expr, &mut output, &resolver).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "(BitVec.add (BitVec.ofNat 8 10) (BitVec.ofNat 8 20))"
        );
    }

    #[test]
    fn test_emit_shift_left() {
        let expr = Expr::ShiftLeft(
            Box::new(Expr::Int(APInt::new(8, 1))),
            Box::new(Expr::Int(APInt::new(8, 3))),
        );
        let mut output = Vec::new();
        let resolver = TestResolver;

        emit(&expr, &mut output, &resolver).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "(BitVec.shiftLeft (BitVec.ofNat 8 1) (BitVec.ofNat 8 3))"
        );
    }

    #[test]
    fn test_emit_if() {
        let expr = Expr::If {
            cond: Box::new(Expr::Bool(true)),
            then: Box::new(Expr::Int(APInt::new(8, 1))),
            else_: Box::new(Expr::Int(APInt::new(8, 0))),
        };
        let mut output = Vec::new();
        let resolver = TestResolver;

        emit(&expr, &mut output, &resolver).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "if true then (BitVec.ofNat 8 1) else (BitVec.ofNat 8 0)"
        );
    }

    #[test]
    fn test_emit_nested() {
        let expr = Expr::Add(
            Box::new(Expr::Symbol(0)),
            Box::new(Expr::Mul(
                Box::new(Expr::Int(APInt::new(8, 2))),
                Box::new(Expr::Int(APInt::new(8, 3))),
            )),
        );
        let mut output = Vec::new();
        let resolver = TestResolver;

        emit(&expr, &mut output, &resolver).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "(BitVec.add reg0 (tir_utils.mul (BitVec.ofNat 8 2) (BitVec.ofNat 8 3)))"
        );
    }

    #[test]
    fn test_emit_mul() {
        let expr = Expr::Mul(
            Box::new(Expr::Int(APInt::new(8, 10))),
            Box::new(Expr::Int(APInt::new(8, 20))),
        );
        let mut output = Vec::new();
        let resolver = TestResolver;

        emit(&expr, &mut output, &resolver).unwrap();
        // Uses tir_utils.mul which handles N+M width multiplication
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "(tir_utils.mul (BitVec.ofNat 8 10) (BitVec.ofNat 8 20))"
        );
    }

    #[test]
    fn test_emit_extract() {
        let expr = Expr::Extract {
            input: Box::new(Expr::Int(APInt::new(8, 0xFF))),
            high: Box::new(Expr::Int(APInt::new(8, 3))),
            low: Box::new(Expr::Int(APInt::new(8, 0))),
        };
        let mut output = Vec::new();
        let resolver = TestResolver;

        emit(&expr, &mut output, &resolver).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "(BitVec.extractLsb (BitVec.ofNat 8 0) (BitVec.ofNat 8 3) (BitVec.ofNat 8 255))"
        );
    }

    #[test]
    fn test_emit_bitvec() {
        let expr = Expr::Bits(BitVec::from_u128(16, 0xABCD));
        let mut output = Vec::new();
        let resolver = TestResolver;

        emit(&expr, &mut output, &resolver).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "(BitVec.ofNat 16 43981)"
        );
    }

    #[test]
    fn test_emit_int_to_bits() {
        let expr = Expr::IntToBits(Box::new(Expr::Int(APInt::new(8, 42))));
        let mut output = Vec::new();
        let resolver = TestResolver;

        emit(&expr, &mut output, &resolver).unwrap();
        // IntToBits is a no-op, should just emit the int
        assert_eq!(String::from_utf8(output).unwrap(), "(BitVec.ofNat 8 42)");
    }

    #[test]
    fn test_emit_bits_to_int() {
        let expr = Expr::BitsToInt {
            bits: Box::new(Expr::Bits(BitVec::from_u128(8, 42))),
            width: 8,
            signed: false,
        };
        let mut output = Vec::new();
        let resolver = TestResolver;

        emit(&expr, &mut output, &resolver).unwrap();
        // BitsToInt is a no-op, should just emit the bits
        assert_eq!(String::from_utf8(output).unwrap(), "(BitVec.ofNat 8 42)");
    }

    #[test]
    fn test_emit_large_bitvec() {
        // Test a 256-bit BitVec
        let mut bytes = vec![0u8; 32];
        bytes[0] = 0xFF; // Set lower byte
        bytes[31] = 0xAB; // Set upper byte
        let expr = Expr::Bits(BitVec::from_bytes(256, &bytes));
        let mut output = Vec::new();
        let resolver = TestResolver;

        emit(&expr, &mut output, &resolver).unwrap();
        let result = String::from_utf8(output).unwrap();
        assert!(result.starts_with("(BitVec.ofNat 256 0x"));
        assert!(result.contains("ab")); // Upper byte
        assert!(result.ends_with("ff)")); // Lower byte
    }

    #[test]
    fn test_emit_zext() {
        let expr = Expr::ZExt {
            input: Box::new(Expr::Int(APInt::new(8, 0xFF))),
            width: Box::new(Expr::Int(APInt::new(8, 16))),
        };
        let mut output = Vec::new();
        let resolver = TestResolver;

        emit(&expr, &mut output, &resolver).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "(BitVec.zeroExtend 16 (BitVec.ofNat 8 255))"
        );
    }

    #[test]
    fn test_emit_sext() {
        let expr = Expr::SExt {
            input: Box::new(Expr::Int(APInt::new_signed(8, -1))),
            width: Box::new(Expr::Int(APInt::new(8, 16))),
        };
        let mut output = Vec::new();
        let resolver = TestResolver;

        emit(&expr, &mut output, &resolver).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "(BitVec.signExtend 16 (BitVec.ofNat 8 255))"
        );
    }
}
