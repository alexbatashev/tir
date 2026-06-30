//! Oracle tests: random width-4 QF_BV formulas bit-blasted, solved, and checked
//! against a direct SMT-LIB reference evaluator (including div-by-zero rules).

use super::*;
use crate::smtlib::convert::lower_script;
use crate::smtlib::parser::parse_script;

const W: u32 = 4;
const MASK: u64 = 0xf;

// ----- reference semantics (the oracle) -----

fn sign(a: u64) -> bool {
    (a >> (W - 1)) & 1 == 1
}

fn neg(a: u64) -> u64 {
    a.wrapping_neg() & MASK
}

fn bvudiv(a: u64, b: u64) -> u64 {
    if b == 0 { MASK } else { (a / b) & MASK }
}

fn bvurem(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { (a % b) & MASK }
}

fn bvsdiv(a: u64, b: u64) -> u64 {
    match (sign(a), sign(b)) {
        (false, false) => bvudiv(a, b),
        (true, false) => neg(bvudiv(neg(a), b)),
        (false, true) => neg(bvudiv(a, neg(b))),
        (true, true) => bvudiv(neg(a), neg(b)),
    }
}

fn bvsrem(a: u64, b: u64) -> u64 {
    match (sign(a), sign(b)) {
        (false, false) => bvurem(a, b),
        (true, false) => neg(bvurem(neg(a), b)),
        (false, true) => bvurem(a, neg(b)),
        (true, true) => neg(bvurem(neg(a), neg(b))),
    }
}

fn shl(a: u64, s: u64) -> u64 {
    if s >= W as u64 { 0 } else { (a << s) & MASK }
}

fn lshr(a: u64, s: u64) -> u64 {
    if s >= W as u64 { 0 } else { a >> s }
}

fn ashr(a: u64, s: u64) -> u64 {
    let fill = if sign(a) { MASK } else { 0 };
    if s >= W as u64 {
        fill
    } else {
        let shifted = a >> s;
        let high = (fill << (W as u64 - s)) & MASK;
        shifted | high
    }
}

#[derive(Clone, Copy)]
enum Bin {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
    Udiv,
    Urem,
    Sdiv,
    Srem,
    Shl,
    Lshr,
    Ashr,
}

#[derive(Clone, Copy)]
enum Un {
    Not,
    Neg,
}

#[derive(Clone, Copy)]
enum Cmp {
    Eq,
    Ne,
    Ult,
    Ule,
    Ugt,
    Uge,
    Slt,
    Sle,
    Sgt,
    Sge,
}

enum E {
    Var(u8),
    Const(u64),
    Bin(Bin, Box<E>, Box<E>),
    Un(Un, Box<E>),
}

enum P {
    Cmp(Cmp, E, E),
    And(Box<P>, Box<P>),
    Or(Box<P>, Box<P>),
    Not(Box<P>),
}

impl E {
    fn eval(&self, x: u64, y: u64) -> u64 {
        match self {
            E::Var(0) => x,
            E::Var(_) => y,
            E::Const(c) => *c & MASK,
            E::Un(Un::Not, a) => !a.eval(x, y) & MASK,
            E::Un(Un::Neg, a) => neg(a.eval(x, y)),
            E::Bin(op, a, b) => {
                let (a, b) = (a.eval(x, y), b.eval(x, y));
                match op {
                    Bin::Add => a.wrapping_add(b) & MASK,
                    Bin::Sub => a.wrapping_sub(b) & MASK,
                    Bin::Mul => a.wrapping_mul(b) & MASK,
                    Bin::And => a & b,
                    Bin::Or => a | b,
                    Bin::Xor => a ^ b,
                    Bin::Udiv => bvudiv(a, b),
                    Bin::Urem => bvurem(a, b),
                    Bin::Sdiv => bvsdiv(a, b),
                    Bin::Srem => bvsrem(a, b),
                    Bin::Shl => shl(a, b),
                    Bin::Lshr => lshr(a, b),
                    Bin::Ashr => ashr(a, b),
                }
            }
        }
    }

    fn smt(&self) -> String {
        match self {
            E::Var(0) => "x".into(),
            E::Var(_) => "y".into(),
            E::Const(c) => format!("#x{:x}", c & MASK),
            E::Un(op, a) => {
                let n = match op {
                    Un::Not => "bvnot",
                    Un::Neg => "bvneg",
                };
                format!("({n} {})", a.smt())
            }
            E::Bin(op, a, b) => {
                let n = match op {
                    Bin::Add => "bvadd",
                    Bin::Sub => "bvsub",
                    Bin::Mul => "bvmul",
                    Bin::And => "bvand",
                    Bin::Or => "bvor",
                    Bin::Xor => "bvxor",
                    Bin::Udiv => "bvudiv",
                    Bin::Urem => "bvurem",
                    Bin::Sdiv => "bvsdiv",
                    Bin::Srem => "bvsrem",
                    Bin::Shl => "bvshl",
                    Bin::Lshr => "bvlshr",
                    Bin::Ashr => "bvashr",
                };
                format!("({n} {} {})", a.smt(), b.smt())
            }
        }
    }
}

impl P {
    fn eval(&self, x: u64, y: u64) -> bool {
        match self {
            P::And(a, b) => a.eval(x, y) && b.eval(x, y),
            P::Or(a, b) => a.eval(x, y) || b.eval(x, y),
            P::Not(a) => !a.eval(x, y),
            P::Cmp(op, a, b) => {
                let (a, b) = (a.eval(x, y), b.eval(x, y));
                let si = |v: u64| if sign(v) { v as i64 - 16 } else { v as i64 };
                match op {
                    Cmp::Eq => a == b,
                    Cmp::Ne => a != b,
                    Cmp::Ult => a < b,
                    Cmp::Ule => a <= b,
                    Cmp::Ugt => a > b,
                    Cmp::Uge => a >= b,
                    Cmp::Slt => si(a) < si(b),
                    Cmp::Sle => si(a) <= si(b),
                    Cmp::Sgt => si(a) > si(b),
                    Cmp::Sge => si(a) >= si(b),
                }
            }
        }
    }

    fn smt(&self) -> String {
        match self {
            P::And(a, b) => format!("(and {} {})", a.smt(), b.smt()),
            P::Or(a, b) => format!("(or {} {})", a.smt(), b.smt()),
            P::Not(a) => format!("(not {})", a.smt()),
            P::Cmp(op, a, b) => {
                let n = match op {
                    Cmp::Eq => "=",
                    Cmp::Ne => "distinct",
                    Cmp::Ult => "bvult",
                    Cmp::Ule => "bvule",
                    Cmp::Ugt => "bvugt",
                    Cmp::Uge => "bvuge",
                    Cmp::Slt => "bvslt",
                    Cmp::Sle => "bvsle",
                    Cmp::Sgt => "bvsgt",
                    Cmp::Sge => "bvsge",
                };
                format!("({n} {} {})", a.smt(), b.smt())
            }
        }
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn gen_expr(rng: &mut Rng, depth: u32) -> E {
    if depth == 0 || rng.below(3) == 0 {
        return match rng.below(3) {
            0 => E::Var(0),
            1 => E::Var(1),
            _ => E::Const(rng.below(16)),
        };
    }
    if rng.below(3) == 0 {
        let op = [Un::Not, Un::Neg][rng.below(2) as usize];
        E::Un(op, Box::new(gen_expr(rng, depth - 1)))
    } else {
        let ops = [
            Bin::Add,
            Bin::Sub,
            Bin::Mul,
            Bin::And,
            Bin::Or,
            Bin::Xor,
            Bin::Udiv,
            Bin::Urem,
            Bin::Sdiv,
            Bin::Srem,
            Bin::Shl,
            Bin::Lshr,
            Bin::Ashr,
        ];
        let op = ops[rng.below(ops.len() as u64) as usize];
        E::Bin(
            op,
            Box::new(gen_expr(rng, depth - 1)),
            Box::new(gen_expr(rng, depth - 1)),
        )
    }
}

fn gen_pred(rng: &mut Rng, depth: u32) -> P {
    if depth == 0 || rng.below(2) == 0 {
        let ops = [
            Cmp::Eq,
            Cmp::Ne,
            Cmp::Ult,
            Cmp::Ule,
            Cmp::Ugt,
            Cmp::Uge,
            Cmp::Slt,
            Cmp::Sle,
            Cmp::Sgt,
            Cmp::Sge,
        ];
        let op = ops[rng.below(ops.len() as u64) as usize];
        return P::Cmp(op, gen_expr(rng, 2), gen_expr(rng, 2));
    }
    match rng.below(3) {
        0 => P::Not(Box::new(gen_pred(rng, depth - 1))),
        1 => P::And(
            Box::new(gen_pred(rng, depth - 1)),
            Box::new(gen_pred(rng, depth - 1)),
        ),
        _ => P::Or(
            Box::new(gen_pred(rng, depth - 1)),
            Box::new(gen_pred(rng, depth - 1)),
        ),
    }
}

fn solve_src(src: &str) -> (SolveOutcome, Vec<crate::smtlib::convert::SymbolInfo>) {
    let script = parse_script(src).unwrap();
    let lo = lower_script::<()>(&script).unwrap();
    let blasted = blast(&lo.graph, &lo.widths).unwrap();
    (blasted.solve(), lo.symbols)
}

fn read(
    model: &std::collections::HashMap<u32, Vec<bool>>,
    symbols: &[crate::smtlib::convert::SymbolInfo],
    name: &str,
) -> u64 {
    let Some(sid) = symbols.iter().position(|s| s.name == name) else {
        return 0;
    };
    match model.get(&(sid as u32)) {
        Some(bits) => bits
            .iter()
            .enumerate()
            .fold(0u64, |acc, (i, &b)| acc | ((b as u64) << i)),
        None => 0,
    }
}

#[test]
fn random_formulas_match_reference() {
    let mut rng = Rng(0xC0FFEE);
    for _ in 0..600 {
        let pred = gen_pred(&mut rng, 3);
        let src = format!(
            "(declare-const x (_ BitVec 4))(declare-const y (_ BitVec 4))(assert {})",
            pred.smt()
        );
        let (outcome, symbols) = solve_src(&src);
        let oracle_sat = (0..16).any(|x| (0..16).any(|y| pred.eval(x, y)));
        match outcome {
            SolveOutcome::Sat(model) => {
                assert!(oracle_sat, "solver sat but unsat by reference:\n{src}");
                let x = read(&model, &symbols, "x");
                let y = read(&model, &symbols, "y");
                assert!(pred.eval(x, y), "returned model is not a model:\n{src}");
            }
            SolveOutcome::Unsat => {
                assert!(!oracle_sat, "solver unsat but sat by reference:\n{src}")
            }
            SolveOutcome::Unknown => panic!("no budget set; unknown impossible"),
        }
    }
}

/// Width-changing ops (concat/extract/extend/ite) the random generator never produces.
#[test]
fn width_changing_ops() {
    let sat = |src: &str| matches!(solve_src(src).0, SolveOutcome::Sat(_));

    // concat: x is the high nibble, y the low nibble.
    let (out, syms) = solve_src(
        "(declare-const x (_ BitVec 4))(declare-const y (_ BitVec 4))\
         (assert (= (concat x y) #xab))",
    );
    match out {
        SolveOutcome::Sat(m) => {
            assert_eq!(read(&m, &syms, "x"), 0xa);
            assert_eq!(read(&m, &syms, "y"), 0xb);
        }
        _ => panic!("concat should be sat"),
    }

    // extract the high nibble of an 8-bit value.
    assert!(sat(
        "(declare-const z (_ BitVec 8))(assert (= ((_ extract 7 4) z) #xc))"
    ));

    // zero_extend keeps the high bits zero; sign_extend copies the sign.
    assert!(sat(
        "(declare-const x (_ BitVec 4))(assert (= ((_ zero_extend 4) x) #x0f))"
    ));
    assert!(!sat(
        "(declare-const x (_ BitVec 4))(assert (= ((_ zero_extend 4) x) #xff))"
    ));
    assert!(sat(
        "(declare-const x (_ BitVec 4))(assert (= ((_ sign_extend 4) x) #xff))"
    ));
    assert!(!sat(
        "(declare-const x (_ BitVec 4))(assert (= ((_ sign_extend 4) x) #x0f))"
    ));

    // ite selects by a boolean condition.
    assert!(sat(
        "(declare-const x (_ BitVec 4))(assert (= (ite (bvult x #x8) #x1 #x2) #x2))"
    ));
}
