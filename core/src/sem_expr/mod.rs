mod apfloat;
mod apint;
mod bitvec;
pub mod lean;
pub mod rocq;
mod simplification;
pub mod smtlib;

pub use apfloat::APFloat;
pub use apint::APInt;
pub use bitvec::BitVec;
pub use simplification::simplify;

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Int(APInt),
    Float(APFloat),
    Bits(BitVec),
    Bool(bool),
    Symbol(u32),
    If {
        cond: Box<Expr>,
        then: Box<Expr>,
        else_: Box<Expr>,
    },
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    UDiv(Box<Expr>, Box<Expr>),
    Eq(Box<Expr>, Box<Expr>),
    Ne(Box<Expr>, Box<Expr>),
    Lt(Box<Expr>, Box<Expr>),
    Le(Box<Expr>, Box<Expr>),
    Gt(Box<Expr>, Box<Expr>),
    Ge(Box<Expr>, Box<Expr>),
    ULt(Box<Expr>, Box<Expr>),
    ULe(Box<Expr>, Box<Expr>),
    UGt(Box<Expr>, Box<Expr>),
    UGe(Box<Expr>, Box<Expr>),
    ShiftLeft(Box<Expr>, Box<Expr>),
    ShiftRightLogic(Box<Expr>, Box<Expr>),
    ShiftRightArithmetic(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Xor(Box<Expr>, Box<Expr>),
    Clamp {
        input: Box<Expr>,
        min: Box<Expr>,
        max: Box<Expr>,
    },
    Extract {
        input: Box<Expr>,
        high: Box<Expr>,
        low: Box<Expr>,
    },
    // Extension operations
    ZExt {
        input: Box<Expr>,
        width: Box<Expr>,
    },
    SExt {
        input: Box<Expr>,
        width: Box<Expr>,
    },
    // Float-specific operations
    Sqrt(Box<Expr>),
    Fma {
        a: Box<Expr>,
        b: Box<Expr>,
        c: Box<Expr>,
    },
    // Reinterpret cast operations
    IntToBits(Box<Expr>),
    FloatToBits(Box<Expr>),
    BitsToInt {
        bits: Box<Expr>,
        width: u32,
        signed: bool,
    },
    BitsToFloat {
        bits: Box<Expr>,
        exp_width: u32,
        mant_width: u32,
        explicit_leading_bit: bool,
    },
}

/// Evaluate an expression to a concrete value
/// Returns an Expr that is either Int, Float, Bits, or Bool (fully evaluated)
/// Panics if the expression contains unbound symbols
pub fn evaluate(expr: Expr) -> Expr {
    match expr {
        // Base cases - already evaluated
        Expr::Int(_) | Expr::Float(_) | Expr::Bits(_) | Expr::Bool(_) => expr,

        // Symbols cannot be evaluated without a binding environment
        Expr::Symbol(_) => panic!("Cannot evaluate expression with unbound symbols"),

        // Conditional
        Expr::If { cond, then, else_ } => {
            let cond_val = evaluate(*cond);
            match cond_val {
                Expr::Bool(true) => evaluate(*then),
                Expr::Bool(false) => evaluate(*else_),
                Expr::Int(i) => {
                    // Non-zero is true, zero is false
                    if i.is_zero() {
                        evaluate(*else_)
                    } else {
                        evaluate(*then)
                    }
                }
                _ => panic!("If condition must evaluate to Bool or Int"),
            }
        }

        // Arithmetic operations
        Expr::Add(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Int(APInt::add(&a, &b)),
                (Expr::Float(a), Expr::Float(b)) => Expr::Float(a.add(&b)),
                _ => panic!("Add requires two Int or two Float operands"),
            }
        }

        Expr::Sub(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Int(APInt::sub(&a, &b)),
                (Expr::Float(a), Expr::Float(b)) => Expr::Float(a.sub(&b)),
                _ => panic!("Sub requires two Int or two Float operands"),
            }
        }

        Expr::Mul(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Int(APInt::mul(&a, &b)),
                (Expr::Float(a), Expr::Float(b)) => Expr::Float(a.mul(&b)),
                _ => panic!("Mul requires two Int or two Float operands"),
            }
        }

        Expr::Div(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => {
                    // Default integer division is signed.
                    Expr::Int(a.sdiv(&b))
                }
                (Expr::Float(a), Expr::Float(b)) => Expr::Float(a.div(&b)),
                _ => panic!("Div requires two Int or two Float operands"),
            }
        }

        Expr::UDiv(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Int(a.udiv(&b)),
                _ => panic!("UDiv requires two Int operands"),
            }
        }

        // Comparison operations
        Expr::Eq(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a == b),
                (Expr::Float(a), Expr::Float(b)) => Expr::Bool(a.eq(&b)),
                (Expr::Bool(a), Expr::Bool(b)) => Expr::Bool(a == b),
                _ => panic!("Eq requires matching operand types"),
            }
        }

        Expr::Ne(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a != b),
                (Expr::Float(a), Expr::Float(b)) => Expr::Bool(!a.eq(&b)),
                (Expr::Bool(a), Expr::Bool(b)) => Expr::Bool(a != b),
                _ => panic!("Ne requires matching operand types"),
            }
        }

        Expr::Lt(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a.slt(&b)),
                (Expr::Float(a), Expr::Float(b)) => Expr::Bool(a.lt(&b)),
                _ => panic!("Lt requires two Int or two Float operands"),
            }
        }

        Expr::Le(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a.sle(&b)),
                (Expr::Float(a), Expr::Float(b)) => Expr::Bool(a.le(&b)),
                _ => panic!("Le requires two Int or two Float operands"),
            }
        }

        Expr::Gt(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a.sgt(&b)),
                (Expr::Float(a), Expr::Float(b)) => Expr::Bool(a.gt(&b)),
                _ => panic!("Gt requires two Int or two Float operands"),
            }
        }

        Expr::Ge(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a.sge(&b)),
                (Expr::Float(a), Expr::Float(b)) => Expr::Bool(a.ge(&b)),
                _ => panic!("Ge requires two Int or two Float operands"),
            }
        }

        Expr::ULt(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a.ult(&b)),
                _ => panic!("ULt requires two Int operands"),
            }
        }

        Expr::ULe(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a.ule(&b)),
                _ => panic!("ULe requires two Int operands"),
            }
        }

        Expr::UGt(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a.ugt(&b)),
                _ => panic!("UGt requires two Int operands"),
            }
        }

        Expr::UGe(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Bool(a.uge(&b)),
                _ => panic!("UGe requires two Int operands"),
            }
        }

        // Shift operations
        Expr::ShiftLeft(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => {
                    let shift_amount = b.to_u64() as u32;
                    Expr::Int(a.shl(shift_amount))
                }
                _ => panic!("ShiftLeft requires two Int operands"),
            }
        }

        Expr::ShiftRightLogic(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => {
                    let shift_amount = b.to_u64() as u32;
                    Expr::Int(a.lshr(shift_amount))
                }
                _ => panic!("ShiftRightLogic requires two Int operands"),
            }
        }

        Expr::ShiftRightArithmetic(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => {
                    let shift_amount = b.to_u64() as u32;
                    Expr::Int(a.ashr(shift_amount))
                }
                _ => panic!("ShiftRightArithmetic requires two Int operands"),
            }
        }

        // Bitwise operations
        Expr::Or(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Int(APInt::or(&a, &b)),
                (Expr::Bool(a), Expr::Bool(b)) => Expr::Bool(a || b),
                _ => panic!("Or requires two Int or two Bool operands"),
            }
        }

        Expr::And(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Int(APInt::and(&a, &b)),
                (Expr::Bool(a), Expr::Bool(b)) => Expr::Bool(a && b),
                _ => panic!("And requires two Int or two Bool operands"),
            }
        }

        Expr::Xor(lhs, rhs) => {
            let lhs_val = evaluate(*lhs);
            let rhs_val = evaluate(*rhs);
            match (lhs_val, rhs_val) {
                (Expr::Int(a), Expr::Int(b)) => Expr::Int(APInt::xor(&a, &b)),
                (Expr::Bool(a), Expr::Bool(b)) => Expr::Bool(a ^ b),
                _ => panic!("Xor requires two Int or two Bool operands"),
            }
        }

        // Clamp operation
        Expr::Clamp { input, min, max } => {
            let input_val = evaluate(*input);
            let min_val = evaluate(*min);
            let max_val = evaluate(*max);
            match (input_val, min_val, max_val) {
                (Expr::Int(inp), Expr::Int(min_i), Expr::Int(max_i)) => {
                    // Clamp the input value between min and max
                    let result = if inp.is_signed() {
                        if inp.slt(&min_i) {
                            min_i
                        } else if inp.sgt(&max_i) {
                            max_i
                        } else {
                            inp
                        }
                    } else {
                        if inp.ult(&min_i) {
                            min_i
                        } else if inp.ugt(&max_i) {
                            max_i
                        } else {
                            inp
                        }
                    };
                    Expr::Int(result)
                }
                _ => panic!("Clamp requires three Int operands"),
            }
        }

        // Extract bits operation
        Expr::Extract { input, high, low } => {
            let input_val = evaluate(*input);
            let high_val = evaluate(*high);
            let low_val = evaluate(*low);
            match (input_val, high_val, low_val) {
                (Expr::Int(inp), Expr::Int(h), Expr::Int(l)) => {
                    let high_bit = h.to_u64() as u32;
                    let low_bit = l.to_u64() as u32;
                    Expr::Int(inp.extract_bits(high_bit, low_bit))
                }
                _ => panic!("Extract requires three Int operands"),
            }
        }

        // Extension operations
        Expr::ZExt { input, width } => {
            let input_val = evaluate(*input);
            let width_val = evaluate(*width);
            match (input_val, width_val) {
                (Expr::Int(i), Expr::Int(w)) => Expr::Int(i.zero_extend(w.to_u64() as u32)),
                (Expr::Int(_), _) => panic!("ZExt width must evaluate to Int"),
                _ => panic!("ZExt requires an Int operand"),
            }
        }

        Expr::SExt { input, width } => {
            let input_val = evaluate(*input);
            let width_val = evaluate(*width);
            match (input_val, width_val) {
                (Expr::Int(i), Expr::Int(w)) => Expr::Int(i.sign_extend(w.to_u64() as u32)),
                (Expr::Int(_), _) => panic!("SExt width must evaluate to Int"),
                _ => panic!("SExt requires an Int operand"),
            }
        }

        // Float-specific operations
        Expr::Sqrt(operand) => {
            let operand_val = evaluate(*operand);
            match operand_val {
                Expr::Float(f) => Expr::Float(f.sqrt()),
                _ => panic!("Sqrt requires a Float operand"),
            }
        }

        Expr::Fma { a, b, c } => {
            let a_val = evaluate(*a);
            let b_val = evaluate(*b);
            let c_val = evaluate(*c);
            match (a_val, b_val, c_val) {
                (Expr::Float(a_f), Expr::Float(b_f), Expr::Float(c_f)) => {
                    Expr::Float(a_f.fma(&b_f, &c_f))
                }
                _ => panic!("Fma requires three Float operands"),
            }
        }

        // Reinterpret cast operations
        Expr::IntToBits(int_expr) => {
            let int_val = evaluate(*int_expr);
            match int_val {
                Expr::Int(i) => Expr::Bits(i.to_bitvec()),
                _ => panic!("IntToBits requires an Int operand"),
            }
        }

        Expr::FloatToBits(float_expr) => {
            let float_val = evaluate(*float_expr);
            match float_val {
                Expr::Float(f) => Expr::Bits(f.to_bitvec()),
                _ => panic!("FloatToBits requires a Float operand"),
            }
        }

        Expr::BitsToInt {
            bits,
            width,
            signed,
        } => {
            let bits_val = evaluate(*bits);
            match bits_val {
                Expr::Bits(bv) => Expr::Int(APInt::from_bitvec(width, signed, &bv)),
                _ => panic!("BitsToInt requires a Bits operand"),
            }
        }

        Expr::BitsToFloat {
            bits,
            exp_width,
            mant_width,
            explicit_leading_bit,
        } => {
            let bits_val = evaluate(*bits);
            match bits_val {
                Expr::Bits(bv) => Expr::Float(APFloat::from_bitvec(
                    exp_width,
                    mant_width,
                    explicit_leading_bit,
                    &bv,
                )),
                _ => panic!("BitsToFloat requires a Bits operand"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evaluate_int() {
        let expr = Expr::Int(APInt::new(8, 42));
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 42),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_bool() {
        let expr = Expr::Bool(true);
        let result = evaluate(expr);
        match result {
            Expr::Bool(b) => assert!(b),
            _ => panic!("Expected Bool"),
        }
    }

    #[test]
    fn test_evaluate_float() {
        let expr = Expr::Float(APFloat::from_f32(3.14));
        let result = evaluate(expr);
        match result {
            Expr::Float(f) => assert!((f.to_f32() - 3.14).abs() < 0.001),
            _ => panic!("Expected Float"),
        }
    }

    #[test]
    fn test_evaluate_float_add() {
        let expr = Expr::Add(
            Box::new(Expr::Float(APFloat::from_f32(2.5))),
            Box::new(Expr::Float(APFloat::from_f32(3.5))),
        );
        let result = evaluate(expr);
        match result {
            Expr::Float(f) => assert_eq!(f.to_f32(), 6.0),
            _ => panic!("Expected Float"),
        }
    }

    #[test]
    fn test_evaluate_float_sqrt() {
        let expr = Expr::Sqrt(Box::new(Expr::Float(APFloat::from_f32(16.0))));
        let result = evaluate(expr);
        match result {
            Expr::Float(f) => assert_eq!(f.to_f32(), 4.0),
            _ => panic!("Expected Float"),
        }
    }

    #[test]
    fn test_evaluate_float_fma() {
        // (2.0 * 3.0) + 4.0 = 10.0
        let expr = Expr::Fma {
            a: Box::new(Expr::Float(APFloat::from_f32(2.0))),
            b: Box::new(Expr::Float(APFloat::from_f32(3.0))),
            c: Box::new(Expr::Float(APFloat::from_f32(4.0))),
        };
        let result = evaluate(expr);
        match result {
            Expr::Float(f) => assert_eq!(f.to_f32(), 10.0),
            _ => panic!("Expected Float"),
        }
    }

    #[test]
    fn test_evaluate_add() {
        let expr = Expr::Add(
            Box::new(Expr::Int(APInt::new(8, 10))),
            Box::new(Expr::Int(APInt::new(8, 20))),
        );
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 30),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_sub() {
        let expr = Expr::Sub(
            Box::new(Expr::Int(APInt::new(8, 50))),
            Box::new(Expr::Int(APInt::new(8, 20))),
        );
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 30),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_mul() {
        let expr = Expr::Mul(
            Box::new(Expr::Int(APInt::new(8, 10))),
            Box::new(Expr::Int(APInt::new(8, 20))),
        );
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 200),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_div_unsigned() {
        let expr = Expr::Div(
            Box::new(Expr::Int(APInt::new(8, 100))),
            Box::new(Expr::Int(APInt::new(8, 5))),
        );
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 20),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_div_signed() {
        let expr = Expr::Div(
            Box::new(Expr::Int(APInt::new_signed(8, -100))),
            Box::new(Expr::Int(APInt::new_signed(8, 5))),
        );
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_i64(), -20),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_lt() {
        let expr = Expr::Lt(
            Box::new(Expr::Int(APInt::new(8, 3))),
            Box::new(Expr::Int(APInt::new(8, 7))),
        );
        let result = evaluate(expr);
        match result {
            Expr::Bool(b) => assert!(b),
            _ => panic!("Expected Bool"),
        }
    }

    #[test]
    fn test_evaluate_ge() {
        let expr = Expr::Ge(
            Box::new(Expr::Int(APInt::new(8, 7))),
            Box::new(Expr::Int(APInt::new(8, 7))),
        );
        let result = evaluate(expr);
        match result {
            Expr::Bool(b) => assert!(b),
            _ => panic!("Expected Bool"),
        }
    }

    #[test]
    fn test_evaluate_eq_bool() {
        let expr = Expr::Eq(Box::new(Expr::Bool(true)), Box::new(Expr::Bool(true)));
        let result = evaluate(expr);
        match result {
            Expr::Bool(b) => assert!(b),
            _ => panic!("Expected Bool"),
        }
    }

    #[test]
    fn test_evaluate_unsigned_div() {
        let expr = Expr::UDiv(
            Box::new(Expr::Int(APInt::new(8, 0xFF))),
            Box::new(Expr::Int(APInt::new(8, 2))),
        );
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 127),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_unsigned_lt() {
        let expr = Expr::ULt(
            Box::new(Expr::Int(APInt::new_signed(8, -1))),
            Box::new(Expr::Int(APInt::new(8, 1))),
        );
        let result = evaluate(expr);
        match result {
            Expr::Bool(b) => assert!(!b),
            _ => panic!("Expected Bool"),
        }
    }

    #[test]
    fn test_evaluate_shift_left() {
        let expr = Expr::ShiftLeft(
            Box::new(Expr::Int(APInt::new(8, 0b00001111))),
            Box::new(Expr::Int(APInt::new(8, 2))),
        );
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 0b00111100),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_shift_right_logic() {
        let expr = Expr::ShiftRightLogic(
            Box::new(Expr::Int(APInt::new(8, 0b11110000))),
            Box::new(Expr::Int(APInt::new(8, 2))),
        );
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 0b00111100),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_shift_right_arithmetic() {
        let expr = Expr::ShiftRightArithmetic(
            Box::new(Expr::Int(APInt::new_signed(8, -16))), // 0b11110000
            Box::new(Expr::Int(APInt::new(8, 2))),
        );
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 0b11111100), // Sign extended
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_and_int() {
        let expr = Expr::And(
            Box::new(Expr::Int(APInt::new(8, 0b11110000))),
            Box::new(Expr::Int(APInt::new(8, 0b10101010))),
        );
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 0b10100000),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_or_int() {
        let expr = Expr::Or(
            Box::new(Expr::Int(APInt::new(8, 0b11110000))),
            Box::new(Expr::Int(APInt::new(8, 0b10101010))),
        );
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 0b11111010),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_xor_int() {
        let expr = Expr::Xor(
            Box::new(Expr::Int(APInt::new(8, 0b11110000))),
            Box::new(Expr::Int(APInt::new(8, 0b10101010))),
        );
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 0b01011010),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_and_bool() {
        let expr = Expr::And(Box::new(Expr::Bool(true)), Box::new(Expr::Bool(false)));
        let result = evaluate(expr);
        match result {
            Expr::Bool(b) => assert!(!b),
            _ => panic!("Expected Bool"),
        }
    }

    #[test]
    fn test_evaluate_or_bool() {
        let expr = Expr::Or(Box::new(Expr::Bool(true)), Box::new(Expr::Bool(false)));
        let result = evaluate(expr);
        match result {
            Expr::Bool(b) => assert!(b),
            _ => panic!("Expected Bool"),
        }
    }

    #[test]
    fn test_evaluate_xor_bool() {
        let expr = Expr::Xor(Box::new(Expr::Bool(true)), Box::new(Expr::Bool(true)));
        let result = evaluate(expr);
        match result {
            Expr::Bool(b) => assert!(!b),
            _ => panic!("Expected Bool"),
        }
    }

    #[test]
    fn test_evaluate_if_bool_true() {
        let expr = Expr::If {
            cond: Box::new(Expr::Bool(true)),
            then: Box::new(Expr::Int(APInt::new(8, 42))),
            else_: Box::new(Expr::Int(APInt::new(8, 24))),
        };
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 42),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_if_bool_false() {
        let expr = Expr::If {
            cond: Box::new(Expr::Bool(false)),
            then: Box::new(Expr::Int(APInt::new(8, 42))),
            else_: Box::new(Expr::Int(APInt::new(8, 24))),
        };
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 24),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_if_int_nonzero() {
        let expr = Expr::If {
            cond: Box::new(Expr::Int(APInt::new(8, 5))),
            then: Box::new(Expr::Int(APInt::new(8, 42))),
            else_: Box::new(Expr::Int(APInt::new(8, 24))),
        };
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 42),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_if_int_zero() {
        let expr = Expr::If {
            cond: Box::new(Expr::Int(APInt::new(8, 0))),
            then: Box::new(Expr::Int(APInt::new(8, 42))),
            else_: Box::new(Expr::Int(APInt::new(8, 24))),
        };
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 24),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_clamp_below_min() {
        let expr = Expr::Clamp {
            input: Box::new(Expr::Int(APInt::new(8, 5))),
            min: Box::new(Expr::Int(APInt::new(8, 10))),
            max: Box::new(Expr::Int(APInt::new(8, 100))),
        };
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 10),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_clamp_above_max() {
        let expr = Expr::Clamp {
            input: Box::new(Expr::Int(APInt::new(8, 150))),
            min: Box::new(Expr::Int(APInt::new(8, 10))),
            max: Box::new(Expr::Int(APInt::new(8, 100))),
        };
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 100),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_clamp_in_range() {
        let expr = Expr::Clamp {
            input: Box::new(Expr::Int(APInt::new(8, 50))),
            min: Box::new(Expr::Int(APInt::new(8, 10))),
            max: Box::new(Expr::Int(APInt::new(8, 100))),
        };
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 50),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_extract() {
        let expr = Expr::Extract {
            input: Box::new(Expr::Int(APInt::new(8, 0b11010110))),
            high: Box::new(Expr::Int(APInt::new(8, 5))),
            low: Box::new(Expr::Int(APInt::new(8, 2))),
        };
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 0b0101),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_nested() {
        // (10 + 20) * 2
        let expr = Expr::Mul(
            Box::new(Expr::Add(
                Box::new(Expr::Int(APInt::new(8, 10))),
                Box::new(Expr::Int(APInt::new(8, 20))),
            )),
            Box::new(Expr::Int(APInt::new(8, 2))),
        );
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 60),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_complex_nested() {
        // if (5 > 0) then (10 + 5) else (20 - 5)
        // Using int 5 as truthy condition
        let expr = Expr::If {
            cond: Box::new(Expr::Int(APInt::new(8, 5))),
            then: Box::new(Expr::Add(
                Box::new(Expr::Int(APInt::new(8, 10))),
                Box::new(Expr::Int(APInt::new(8, 5))),
            )),
            else_: Box::new(Expr::Sub(
                Box::new(Expr::Int(APInt::new(8, 20))),
                Box::new(Expr::Int(APInt::new(8, 5))),
            )),
        };
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 15),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_int_to_bits() {
        let expr = Expr::IntToBits(Box::new(Expr::Int(APInt::new(8, 0xAB))));
        let result = evaluate(expr);
        match result {
            Expr::Bits(bv) => {
                assert_eq!(bv.width(), 8);
                assert_eq!(bv.to_u128(), 0xAB);
            }
            _ => panic!("Expected Bits"),
        }
    }

    #[test]
    fn test_evaluate_float_to_bits() {
        let expr = Expr::FloatToBits(Box::new(Expr::Float(APFloat::from_f32(1.0))));
        let result = evaluate(expr);
        match result {
            Expr::Bits(bv) => {
                assert_eq!(bv.width(), 32);
                assert_eq!(bv.to_u128(), 0x3F800000); // IEEE 754 representation of 1.0
            }
            _ => panic!("Expected Bits"),
        }
    }

    #[test]
    fn test_evaluate_bits_to_int() {
        let bits = BitVec::from_u128(8, 0xAB);
        let expr = Expr::BitsToInt {
            bits: Box::new(Expr::Bits(bits)),
            width: 8,
            signed: false,
        };
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 0xAB),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_bits_to_float() {
        let bits = BitVec::from_u128(32, 0x3F800000); // 1.0 in IEEE 754
        let expr = Expr::BitsToFloat {
            bits: Box::new(Expr::Bits(bits)),
            exp_width: 8,
            mant_width: 23,
            explicit_leading_bit: false,
        };
        let result = evaluate(expr);
        match result {
            Expr::Float(f) => assert_eq!(f.to_f32(), 1.0),
            _ => panic!("Expected Float"),
        }
    }

    #[test]
    fn test_evaluate_roundtrip_int_bits() {
        // Test Int -> Bits -> Int roundtrip
        let original = APInt::new(16, 0x1234);
        let expr = Expr::BitsToInt {
            bits: Box::new(Expr::IntToBits(Box::new(Expr::Int(original.clone())))),
            width: 16,
            signed: false,
        };
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => assert_eq!(i.to_u64(), 0x1234),
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_roundtrip_float_bits() {
        // Test Float -> Bits -> Float roundtrip
        let original = APFloat::from_f32(3.14159);
        let expr = Expr::BitsToFloat {
            bits: Box::new(Expr::FloatToBits(Box::new(Expr::Float(original.clone())))),
            exp_width: 8,
            mant_width: 23,
            explicit_leading_bit: false,
        };
        let result = evaluate(expr);
        match result {
            Expr::Float(f) => {
                // Should be very close (within floating point precision)
                assert!((f.to_f32() - 3.14159).abs() < 0.00001);
            }
            _ => panic!("Expected Float"),
        }
    }

    #[test]
    fn test_evaluate_zext() {
        // Zero-extend 8-bit to 16-bit
        let expr = Expr::ZExt {
            input: Box::new(Expr::Int(APInt::new(8, 0xFF))),
            width: Box::new(Expr::Int(APInt::new(8, 16))),
        };
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => {
                assert_eq!(i.width(), 16);
                assert_eq!(i.to_u64(), 0x00FF);
            }
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_sext_positive() {
        // Sign-extend positive 8-bit to 16-bit
        let expr = Expr::SExt {
            input: Box::new(Expr::Int(APInt::new_signed(8, 42))),
            width: Box::new(Expr::Int(APInt::new(8, 16))),
        };
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => {
                assert_eq!(i.width(), 16);
                assert_eq!(i.to_i64(), 42);
            }
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_evaluate_sext_negative() {
        // Sign-extend negative 8-bit to 16-bit
        // -1 in 8-bit is 0xFF, in 16-bit should be 0xFFFF
        let expr = Expr::SExt {
            input: Box::new(Expr::Int(APInt::new_signed(8, -1))),
            width: Box::new(Expr::Int(APInt::new(8, 16))),
        };
        let result = evaluate(expr);
        match result {
            Expr::Int(i) => {
                assert_eq!(i.width(), 16);
                assert_eq!(i.to_i64(), -1);
                assert_eq!(i.to_u64(), 0xFFFF);
            }
            _ => panic!("Expected Int"),
        }
    }
}
