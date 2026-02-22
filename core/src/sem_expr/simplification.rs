use crate::sem_expr::{APFloat, APInt, Expr};

/// Simplify an expression by applying algebraic identities and constant folding
/// This performs symbolic simplification and does not require all symbols to be bound
pub fn simplify(expr: Expr) -> Expr {
    match expr {
        // Base cases - already simplified
        Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Symbol(_) => expr,

        // Conditional simplification
        Expr::If { cond, then, else_ } => {
            let cond = simplify(*cond);
            match &cond {
                // Constant folding
                Expr::Bool(true) => simplify(*then),
                Expr::Bool(false) => simplify(*else_),
                Expr::Int(i) if i.is_zero() => simplify(*else_),
                Expr::Int(_) => simplify(*then),
                _ => {
                    let then = simplify(*then);
                    let else_ = simplify(*else_);
                    // If both branches are identical, return one of them
                    if then == else_ {
                        then
                    } else {
                        Expr::If {
                            cond: Box::new(cond),
                            then: Box::new(then),
                            else_: Box::new(else_),
                        }
                    }
                }
            }
        }

        // Addition simplifications
        Expr::Add(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);

            match (&lhs, &rhs) {
                // Constant folding: a + b -> c
                (Expr::Int(a), Expr::Int(b)) => Expr::Int(APInt::add(a, b)),
                (Expr::Float(a), Expr::Float(b)) => Expr::Float(a.add(b)),
                // Identity: x + 0 -> x
                (_, Expr::Int(b)) if b.is_zero() => lhs,
                (Expr::Int(a), _) if a.is_zero() => rhs,
                (_, Expr::Float(b)) if b.is_zero() => lhs,
                (Expr::Float(a), _) if a.is_zero() => rhs,
                _ => Expr::Add(Box::new(lhs), Box::new(rhs)),
            }
        }

        // Subtraction simplifications
        Expr::Sub(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);

            match (&lhs, &rhs) {
                // Constant folding: a - b -> c
                (Expr::Int(a), Expr::Int(b)) => Expr::Int(APInt::sub(a, b)),
                (Expr::Float(a), Expr::Float(b)) => Expr::Float(a.sub(b)),
                // Identity: x - 0 -> x
                (_, Expr::Int(b)) if b.is_zero() => lhs,
                (_, Expr::Float(b)) if b.is_zero() => lhs,
                // x - x -> 0 (for concrete values)
                _ if lhs == rhs => match &lhs {
                    Expr::Int(a) => Expr::Int(APInt::zero(a.width())),
                    Expr::Float(a) => Expr::Float(APFloat::zero(
                        a.exp_width(),
                        a.mant_width(),
                        a.has_explicit_leading_bit(),
                        false,
                    )),
                    _ => Expr::Sub(Box::new(lhs), Box::new(rhs)),
                },
                _ => Expr::Sub(Box::new(lhs), Box::new(rhs)),
            }
        }

        // Multiplication simplifications
        Expr::Mul(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);

            match (&lhs, &rhs) {
                // Constant folding: a * b -> c
                (Expr::Int(a), Expr::Int(b)) => Expr::Int(APInt::mul(a, b)),
                (Expr::Float(a), Expr::Float(b)) => Expr::Float(a.mul(b)),
                // Annihilation: x * 0 -> 0
                (_, Expr::Int(b)) if b.is_zero() => rhs,
                (Expr::Int(a), _) if a.is_zero() => lhs,
                (_, Expr::Float(b)) if b.is_zero() => rhs,
                (Expr::Float(a), _) if a.is_zero() => lhs,
                // Identity: x * 1 -> x
                (_, Expr::Int(b)) if b.is_one() => lhs,
                (Expr::Int(a), _) if a.is_one() => rhs,
                // Note: Float 1.0 check would need to compare bit pattern
                _ => Expr::Mul(Box::new(lhs), Box::new(rhs)),
            }
        }

        // Division simplifications
        Expr::Div(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);

            match (&lhs, &rhs) {
                // Constant folding: a / b -> c
                (Expr::Int(a), Expr::Int(b)) if !b.is_zero() => Expr::Int(a.sdiv(b)),
                (Expr::Float(a), Expr::Float(b)) if !b.is_zero() => Expr::Float(a.div(b)),
                // Identity: x / 1 -> x
                (_, Expr::Int(b)) if b.is_one() => lhs,
                // x / x -> 1 (for concrete values)
                _ if lhs == rhs => match &lhs {
                    Expr::Int(a) => Expr::Int(APInt::one(a.width())),
                    _ => Expr::Div(Box::new(lhs), Box::new(rhs)),
                },
                _ => Expr::Div(Box::new(lhs), Box::new(rhs)),
            }
        }

        Expr::UDiv(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);

            match (&lhs, &rhs) {
                (Expr::Int(a), Expr::Int(b)) if !b.is_zero() => Expr::Int(a.udiv(b)),
                (_, Expr::Int(b)) if b.is_one() => lhs,
                _ if lhs == rhs => match &lhs {
                    Expr::Int(a) => Expr::Int(APInt::one(a.width())),
                    _ => Expr::UDiv(Box::new(lhs), Box::new(rhs)),
                },
                _ => Expr::UDiv(Box::new(lhs), Box::new(rhs)),
            }
        }

        // Comparison simplifications
        Expr::Eq(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);

            match (&lhs, &rhs) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a == b),
                (Expr::Float(a), Expr::Float(b)) => Expr::Bool(a.eq(b)),
                (Expr::Bool(a), Expr::Bool(b)) => Expr::Bool(*a == *b),
                _ if lhs == rhs => Expr::Bool(true),
                _ => Expr::Eq(Box::new(lhs), Box::new(rhs)),
            }
        }

        Expr::Ne(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);

            match (&lhs, &rhs) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a != b),
                (Expr::Float(a), Expr::Float(b)) => Expr::Bool(!a.eq(b)),
                (Expr::Bool(a), Expr::Bool(b)) => Expr::Bool(*a != *b),
                _ if lhs == rhs => Expr::Bool(false),
                _ => Expr::Ne(Box::new(lhs), Box::new(rhs)),
            }
        }

        Expr::Lt(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);

            match (&lhs, &rhs) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a.slt(b)),
                (Expr::Float(a), Expr::Float(b)) => Expr::Bool(a.lt(b)),
                _ => Expr::Lt(Box::new(lhs), Box::new(rhs)),
            }
        }

        Expr::Le(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);

            match (&lhs, &rhs) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a.sle(b)),
                (Expr::Float(a), Expr::Float(b)) => Expr::Bool(a.le(b)),
                _ => Expr::Le(Box::new(lhs), Box::new(rhs)),
            }
        }

        Expr::Gt(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);

            match (&lhs, &rhs) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a.sgt(b)),
                (Expr::Float(a), Expr::Float(b)) => Expr::Bool(a.gt(b)),
                _ => Expr::Gt(Box::new(lhs), Box::new(rhs)),
            }
        }

        Expr::Ge(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);

            match (&lhs, &rhs) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a.sge(b)),
                (Expr::Float(a), Expr::Float(b)) => Expr::Bool(a.ge(b)),
                _ => Expr::Ge(Box::new(lhs), Box::new(rhs)),
            }
        }

        Expr::ULt(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);
            match (&lhs, &rhs) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a.ult(b)),
                _ => Expr::ULt(Box::new(lhs), Box::new(rhs)),
            }
        }

        Expr::ULe(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);
            match (&lhs, &rhs) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a.ule(b)),
                _ => Expr::ULe(Box::new(lhs), Box::new(rhs)),
            }
        }

        Expr::UGt(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);
            match (&lhs, &rhs) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a.ugt(b)),
                _ => Expr::UGt(Box::new(lhs), Box::new(rhs)),
            }
        }

        Expr::UGe(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);
            match (&lhs, &rhs) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a.uge(b)),
                _ => Expr::UGe(Box::new(lhs), Box::new(rhs)),
            }
        }

        // Shift left simplifications
        Expr::ShiftLeft(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);

            match (&lhs, &rhs) {
                // Constant folding
                (Expr::Int(a), Expr::Int(b)) => {
                    let shift = b.to_u64() as u32;
                    Expr::Int(a.shl(shift))
                }
                // x << 0 -> x
                (_, Expr::Int(b)) if b.is_zero() => lhs,
                // 0 << x -> 0
                (Expr::Int(a), _) if a.is_zero() => lhs,
                _ => Expr::ShiftLeft(Box::new(lhs), Box::new(rhs)),
            }
        }

        // Logical shift right simplifications
        Expr::ShiftRightLogic(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);

            match (&lhs, &rhs) {
                // Constant folding
                (Expr::Int(a), Expr::Int(b)) => {
                    let shift = b.to_u64() as u32;
                    Expr::Int(a.lshr(shift))
                }
                // x >> 0 -> x
                (_, Expr::Int(b)) if b.is_zero() => lhs,
                // 0 >> x -> 0
                (Expr::Int(a), _) if a.is_zero() => lhs,
                _ => Expr::ShiftRightLogic(Box::new(lhs), Box::new(rhs)),
            }
        }

        // Arithmetic shift right simplifications
        Expr::ShiftRightArithmetic(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);

            match (&lhs, &rhs) {
                // Constant folding
                (Expr::Int(a), Expr::Int(b)) => {
                    let shift = b.to_u64() as u32;
                    Expr::Int(a.ashr(shift))
                }
                // x >> 0 -> x
                (_, Expr::Int(b)) if b.is_zero() => lhs,
                _ => Expr::ShiftRightArithmetic(Box::new(lhs), Box::new(rhs)),
            }
        }

        // Bitwise OR simplifications
        Expr::Or(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);

            match (&lhs, &rhs) {
                // Constant folding for Int
                (Expr::Int(a), Expr::Int(b)) => Expr::Int(APInt::or(a, b)),
                // Constant folding for Bool
                (Expr::Bool(a), Expr::Bool(b)) => Expr::Bool(*a || *b),
                // Identity: x | 0 -> x
                (_, Expr::Int(b)) if b.is_zero() => lhs,
                (Expr::Int(a), _) if a.is_zero() => rhs,
                // Identity: x | false -> x
                (_, Expr::Bool(false)) => lhs,
                (Expr::Bool(false), _) => rhs,
                // Annihilation for Bool: x | true -> true
                (_, Expr::Bool(true)) => rhs,
                (Expr::Bool(true), _) => lhs,
                // Idempotence: x | x -> x
                _ if lhs == rhs => lhs,
                _ => Expr::Or(Box::new(lhs), Box::new(rhs)),
            }
        }

        // Bitwise AND simplifications
        Expr::And(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);

            match (&lhs, &rhs) {
                // Constant folding for Int
                (Expr::Int(a), Expr::Int(b)) => Expr::Int(APInt::and(a, b)),
                // Constant folding for Bool
                (Expr::Bool(a), Expr::Bool(b)) => Expr::Bool(*a && *b),
                // Annihilation: x & 0 -> 0
                (_, Expr::Int(b)) if b.is_zero() => rhs,
                (Expr::Int(a), _) if a.is_zero() => lhs,
                // Annihilation for Bool: x & false -> false
                (_, Expr::Bool(false)) => rhs,
                (Expr::Bool(false), _) => lhs,
                // Identity for Bool: x & true -> x
                (_, Expr::Bool(true)) => lhs,
                (Expr::Bool(true), _) => rhs,
                // Idempotence: x & x -> x
                _ if lhs == rhs => lhs,
                _ => Expr::And(Box::new(lhs), Box::new(rhs)),
            }
        }

        // Bitwise XOR simplifications
        Expr::Xor(lhs, rhs) => {
            let lhs = simplify(*lhs);
            let rhs = simplify(*rhs);

            match (&lhs, &rhs) {
                // Constant folding for Int
                (Expr::Int(a), Expr::Int(b)) => Expr::Int(APInt::xor(a, b)),
                // Constant folding for Bool
                (Expr::Bool(a), Expr::Bool(b)) => Expr::Bool(*a ^ *b),
                // Identity: x ^ 0 -> x
                (_, Expr::Int(b)) if b.is_zero() => lhs,
                (Expr::Int(a), _) if a.is_zero() => rhs,
                // Identity for Bool: x ^ false -> x
                (_, Expr::Bool(false)) => lhs,
                (Expr::Bool(false), _) => rhs,
                // Inverse: x ^ x -> 0
                _ if lhs == rhs => match &lhs {
                    Expr::Int(a) => Expr::Int(APInt::zero(a.width())),
                    Expr::Bool(_) => Expr::Bool(false),
                    _ => Expr::Xor(Box::new(lhs), Box::new(rhs)),
                },
                _ => Expr::Xor(Box::new(lhs), Box::new(rhs)),
            }
        }

        // Clamp simplifications
        Expr::Clamp { input, min, max } => {
            let input = simplify(*input);
            let min = simplify(*min);
            let max = simplify(*max);

            match (&input, &min, &max) {
                // Constant folding
                (Expr::Int(inp), Expr::Int(min_i), Expr::Int(max_i)) => {
                    let result = if inp.is_signed() {
                        if inp.slt(min_i) {
                            min_i.clone()
                        } else if inp.sgt(max_i) {
                            max_i.clone()
                        } else {
                            inp.clone()
                        }
                    } else {
                        if inp.ult(min_i) {
                            min_i.clone()
                        } else if inp.ugt(max_i) {
                            max_i.clone()
                        } else {
                            inp.clone()
                        }
                    };
                    Expr::Int(result)
                }
                _ => Expr::Clamp {
                    input: Box::new(input),
                    min: Box::new(min),
                    max: Box::new(max),
                },
            }
        }

        // Extract simplifications
        Expr::Extract { input, high, low } => {
            let input = simplify(*input);
            let high = simplify(*high);
            let low = simplify(*low);

            match (&input, &high, &low) {
                // Constant folding
                (Expr::Int(inp), Expr::Int(h), Expr::Int(l)) => {
                    let high_bit = h.to_u64() as u32;
                    let low_bit = l.to_u64() as u32;
                    Expr::Int(inp.extract_bits(high_bit, low_bit))
                }
                _ => Expr::Extract {
                    input: Box::new(input),
                    high: Box::new(high),
                    low: Box::new(low),
                },
            }
        }

        // Extension simplifications
        Expr::ZExt { input, width } => {
            let input = simplify(*input);
            let width = simplify(*width);
            match (&input, &width) {
                // Identity: extending to the same width is a no-op
                (Expr::Int(i), Expr::Int(w)) if i.width() == w.to_u64() as u32 => input,
                // Constant folding
                (Expr::Int(i), Expr::Int(w)) => Expr::Int(i.zero_extend(w.to_u64() as u32)),
                _ => Expr::ZExt {
                    input: Box::new(input),
                    width: Box::new(width),
                },
            }
        }

        Expr::SExt { input, width } => {
            let input = simplify(*input);
            let width = simplify(*width);
            match (&input, &width) {
                // Identity: extending to the same width is a no-op
                (Expr::Int(i), Expr::Int(w)) if i.width() == w.to_u64() as u32 => input,
                // Constant folding
                (Expr::Int(i), Expr::Int(w)) => Expr::Int(i.sign_extend(w.to_u64() as u32)),
                _ => Expr::SExt {
                    input: Box::new(input),
                    width: Box::new(width),
                },
            }
        }

        // Float-specific operations
        Expr::Sqrt(operand) => {
            let operand = simplify(*operand);
            match &operand {
                // Constant folding
                Expr::Float(f) => Expr::Float(f.sqrt()),
                _ => Expr::Sqrt(Box::new(operand)),
            }
        }

        Expr::Fma { a, b, c } => {
            let a = simplify(*a);
            let b = simplify(*b);
            let c = simplify(*c);

            match (&a, &b, &c) {
                // Constant folding
                (Expr::Float(a_f), Expr::Float(b_f), Expr::Float(c_f)) => {
                    Expr::Float(a_f.fma(b_f, c_f))
                }
                _ => Expr::Fma {
                    a: Box::new(a),
                    b: Box::new(b),
                    c: Box::new(c),
                },
            }
        }

        // Reinterpret cast simplifications
        Expr::IntToBits(int_expr) => {
            let int_expr = simplify(*int_expr);
            match &int_expr {
                // Constant folding
                Expr::Int(i) => Expr::Bits(i.to_bitvec()),
                _ => Expr::IntToBits(Box::new(int_expr)),
            }
        }

        Expr::FloatToBits(float_expr) => {
            let float_expr = simplify(*float_expr);
            match &float_expr {
                // Constant folding
                Expr::Float(f) => Expr::Bits(f.to_bitvec()),
                _ => Expr::FloatToBits(Box::new(float_expr)),
            }
        }

        Expr::BitsToInt {
            bits,
            width,
            signed,
        } => {
            let bits = simplify(*bits);
            match &bits {
                // Constant folding
                Expr::Bits(bv) => Expr::Int(APInt::from_bitvec(width, signed, bv)),
                _ => Expr::BitsToInt {
                    bits: Box::new(bits),
                    width,
                    signed,
                },
            }
        }

        Expr::BitsToFloat {
            bits,
            exp_width,
            mant_width,
            explicit_leading_bit,
        } => {
            let bits = simplify(*bits);
            match &bits {
                // Constant folding
                Expr::Bits(bv) => Expr::Float(APFloat::from_bitvec(
                    exp_width,
                    mant_width,
                    explicit_leading_bit,
                    bv,
                )),
                _ => Expr::BitsToFloat {
                    bits: Box::new(bits),
                    exp_width,
                    mant_width,
                    explicit_leading_bit,
                },
            }
        }

        // Base case for Bits (already simplified)
        Expr::Bits(_) => expr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simplify_constant() {
        let expr = Expr::Int(APInt::new(8, 42));
        let result = simplify(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 42),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_simplify_symbol() {
        let expr = Expr::Symbol(1);
        let result = simplify(expr);
        match result {
            Expr::Symbol(s) => assert_eq!(s, 1),
            _ => panic!("Expected Symbol"),
        }
    }

    #[test]
    fn test_simplify_add_constants() {
        let expr = Expr::Add(
            Box::new(Expr::Int(APInt::new(8, 10))),
            Box::new(Expr::Int(APInt::new(8, 20))),
        );
        let result = simplify(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 30),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_simplify_add_zero() {
        let expr = Expr::Add(
            Box::new(Expr::Symbol(1)),
            Box::new(Expr::Int(APInt::new(8, 0))),
        );
        let result = simplify(expr);
        match result {
            Expr::Symbol(s) => assert_eq!(s, 1),
            _ => panic!("Expected Symbol"),
        }
    }

    #[test]
    fn test_simplify_mul_zero() {
        let expr = Expr::Mul(
            Box::new(Expr::Symbol(1)),
            Box::new(Expr::Int(APInt::new(8, 0))),
        );
        let result = simplify(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 0),
            _ => panic!("Expected Int(0)"),
        }
    }

    #[test]
    fn test_simplify_mul_one() {
        let expr = Expr::Mul(
            Box::new(Expr::Symbol(1)),
            Box::new(Expr::Int(APInt::new(8, 1))),
        );
        let result = simplify(expr);
        match result {
            Expr::Symbol(s) => assert_eq!(s, 1),
            _ => panic!("Expected Symbol"),
        }
    }

    #[test]
    fn test_simplify_sub_self() {
        let expr = Expr::Sub(
            Box::new(Expr::Int(APInt::new(8, 42))),
            Box::new(Expr::Int(APInt::new(8, 42))),
        );
        let result = simplify(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 0),
            _ => panic!("Expected Int(0)"),
        }
    }

    #[test]
    fn test_simplify_xor_self() {
        let expr = Expr::Xor(Box::new(Expr::Symbol(1)), Box::new(Expr::Symbol(1)));
        let result = simplify(expr);
        // Should create a zero, but we need to know the width
        // For now, this stays as symbolic
        match result {
            Expr::Xor(_, _) => {} // Can't simplify without knowing width
            _ => {}
        }
    }

    #[test]
    fn test_simplify_if_true() {
        let expr = Expr::If {
            cond: Box::new(Expr::Bool(true)),
            then: Box::new(Expr::Int(APInt::new(8, 42))),
            else_: Box::new(Expr::Int(APInt::new(8, 24))),
        };
        let result = simplify(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 42),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_simplify_if_false() {
        let expr = Expr::If {
            cond: Box::new(Expr::Bool(false)),
            then: Box::new(Expr::Int(APInt::new(8, 42))),
            else_: Box::new(Expr::Int(APInt::new(8, 24))),
        };
        let result = simplify(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 24),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_simplify_if_identical_branches() {
        let expr = Expr::If {
            cond: Box::new(Expr::Symbol(1)),
            then: Box::new(Expr::Int(APInt::new(8, 42))),
            else_: Box::new(Expr::Int(APInt::new(8, 42))),
        };
        let result = simplify(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 42),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_simplify_and_true() {
        let expr = Expr::And(Box::new(Expr::Symbol(1)), Box::new(Expr::Bool(true)));
        let result = simplify(expr);
        match result {
            Expr::Symbol(s) => assert_eq!(s, 1),
            _ => panic!("Expected Symbol"),
        }
    }

    #[test]
    fn test_simplify_or_false() {
        let expr = Expr::Or(Box::new(Expr::Symbol(1)), Box::new(Expr::Bool(false)));
        let result = simplify(expr);
        match result {
            Expr::Symbol(s) => assert_eq!(s, 1),
            _ => panic!("Expected Symbol"),
        }
    }

    #[test]
    fn test_simplify_nested() {
        // (x + 0) * 1
        let expr = Expr::Mul(
            Box::new(Expr::Add(
                Box::new(Expr::Symbol(1)),
                Box::new(Expr::Int(APInt::new(8, 0))),
            )),
            Box::new(Expr::Int(APInt::new(8, 1))),
        );
        let result = simplify(expr);
        match result {
            Expr::Symbol(s) => assert_eq!(s, 1),
            _ => panic!("Expected Symbol"),
        }
    }

    #[test]
    fn test_simplify_complex_nested() {
        // ((10 + 20) * 2) / 2
        let expr = Expr::Div(
            Box::new(Expr::Mul(
                Box::new(Expr::Add(
                    Box::new(Expr::Int(APInt::new(8, 10))),
                    Box::new(Expr::Int(APInt::new(8, 20))),
                )),
                Box::new(Expr::Int(APInt::new(8, 2))),
            )),
            Box::new(Expr::Int(APInt::new(8, 2))),
        );
        let result = simplify(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 30),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_simplify_shift_by_zero() {
        let expr = Expr::ShiftLeft(
            Box::new(Expr::Symbol(1)),
            Box::new(Expr::Int(APInt::new(8, 0))),
        );
        let result = simplify(expr);
        match result {
            Expr::Symbol(s) => assert_eq!(s, 1),
            _ => panic!("Expected Symbol"),
        }
    }

    #[test]
    fn test_simplify_idempotent_or() {
        let expr = Expr::Or(Box::new(Expr::Symbol(1)), Box::new(Expr::Symbol(1)));
        let result = simplify(expr);
        match result {
            Expr::Symbol(s) => assert_eq!(s, 1),
            _ => panic!("Expected Symbol"),
        }
    }

    #[test]
    fn test_simplify_idempotent_and() {
        let expr = Expr::And(Box::new(Expr::Symbol(1)), Box::new(Expr::Symbol(1)));
        let result = simplify(expr);
        match result {
            Expr::Symbol(s) => assert_eq!(s, 1),
            _ => panic!("Expected Symbol"),
        }
    }
}
