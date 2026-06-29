use super::*;
use crate::smtlib::parser::parse_script;

/// Run a script through the driver and return everything it printed.
fn run(src: &str) -> String {
    let script = parse_script(src).expect("parse");
    let mut out = Vec::new();
    run_script(&script, &mut out).expect("run");
    String::from_utf8(out).expect("utf8")
}

fn lines(src: &str) -> Vec<String> {
    run(src).lines().map(str::to_string).collect()
}

#[test]
fn sat_with_counterexample() {
    let out = lines(
        "(declare-const x (_ BitVec 8))\
         (assert (= (bvadd x #x01) #x00))\
         (check-sat)\
         (get-value (x))",
    );
    assert_eq!(out[0], "sat");
    assert_eq!(out[1], "((x #xff))");
}

#[test]
fn plainly_unsat() {
    let out = lines(
        "(declare-const x (_ BitVec 8))\
         (assert (and (bvult x #x05) (bvugt x #x05)))\
         (check-sat)",
    );
    assert_eq!(out, ["unsat"]);
}

#[test]
fn empty_assertions_are_sat() {
    let out = lines("(declare-const x (_ BitVec 8))(check-sat)");
    assert_eq!(out, ["sat"]);
}

#[test]
fn get_model_lists_each_constant() {
    let out = run("(declare-const x (_ BitVec 8))\
         (assert (= x #x2a))\
         (check-sat)\
         (get-model)");
    assert!(out.starts_with("sat\n"));
    assert!(
        out.contains("(define-fun x () (_ BitVec 8) #x2a)"),
        "model was:\n{out}"
    );
}

#[test]
fn boolean_values_print_as_true_false() {
    let out = lines(
        "(declare-const b Bool)\
         (assert b)\
         (check-sat)\
         (get-value (b))",
    );
    assert_eq!(out[0], "sat");
    assert_eq!(out[1], "((b true))");
}

#[test]
fn push_pop_scopes_assertions() {
    let out = lines(
        "(declare-const x (_ BitVec 4))\
         (assert (bvult x #x5))\
         (check-sat)\
         (push 1)\
         (assert (bvugt x #x5))\
         (check-sat)\
         (pop 1)\
         (check-sat)",
    );
    assert_eq!(out, ["sat", "unsat", "sat"]);
}

#[test]
fn unsupported_operator_is_unknown() {
    let out = lines(
        "(declare-const x (_ BitVec 4))(declare-const y (_ BitVec 4))\
         (assert (= (bvsmod x y) x))(check-sat)",
    );
    assert_eq!(out, ["unknown"]);
}

#[test]
fn non_nibble_width_uses_binary() {
    // A 3-bit value cannot use `#x`; it must print as `#b`.
    let out = lines(
        "(declare-const x (_ BitVec 3))\
         (assert (= x #b101))\
         (check-sat)\
         (get-value (x))",
    );
    assert_eq!(out[0], "sat");
    assert_eq!(out[1], "((x #b101))");
}

#[test]
fn get_value_of_expression() {
    let out = lines(
        "(declare-const x (_ BitVec 8))\
         (assert (= x #x10))\
         (check-sat)\
         (get-value ((bvadd x #x01)))",
    );
    assert_eq!(out[0], "sat");
    assert_eq!(out[1], "(((bvadd x #x01) #x11))");
}

#[test]
fn check_sat_assuming_uses_literals() {
    // Prop literals are `symbol` or `(not symbol)`. Under `p` the formula is
    // unsatisfiable; under `(not p)` it is satisfiable.
    let out = lines(
        "(declare-const p Bool)(declare-const q Bool)\
         (assert (=> p q))(assert (not q))\
         (check-sat-assuming (p))\
         (check-sat-assuming ((not p)))",
    );
    assert_eq!(out, ["unsat", "sat"]);
}

#[test]
fn echo_is_passed_through() {
    let out = lines("(echo \"hello\")");
    assert_eq!(out, ["\"hello\""]);
}

#[test]
fn solver_api_direct() {
    let mut s = Solver::new();
    s.declare_const("x".into(), sort_bitvec(8));
    s.assert(parse_assert("(= x #x07)"));
    assert_eq!(s.check_sat(), CheckResult::Sat);
    let model = s.get_value(&[parse_assert("x")]).unwrap();
    assert_eq!(model, "((x #x07))");
}

fn sort_bitvec(n: u128) -> Sort {
    Sort {
        id: Identifier {
            symbol: Symbol("BitVec".into()),
            indices: vec![Index::Numeral(n)],
        },
        params: Vec::new(),
    }
}

fn parse_assert(term_src: &str) -> Term {
    crate::smtlib::parser::parse_term(term_src).expect("term")
}
