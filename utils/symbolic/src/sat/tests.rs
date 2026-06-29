use super::*;

/// A tiny deterministic PRNG so the randomized tests are reproducible without a
/// dependency.
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

fn lit_true(l: Lit, model: &[bool]) -> bool {
    model[l.var().index()] ^ l.is_negated()
}

fn satisfies(clauses: &[Vec<Lit>], model: &[bool]) -> bool {
    clauses
        .iter()
        .all(|c| c.iter().any(|&l| lit_true(l, model)))
}

/// Exhaustively decide satisfiability — the oracle for the random tests.
fn brute(n: usize, clauses: &[Vec<Lit>]) -> bool {
    (0..(1u64 << n)).any(|mask| {
        let model: Vec<bool> = (0..n).map(|i| (mask >> i) & 1 == 1).collect();
        satisfies(clauses, &model)
    })
}

fn solve_clauses(n: usize, clauses: &[Vec<Lit>]) -> SatResult {
    // `new_var` hands out Var(0), Var(1), ... in order, matching the indices the
    // clause literals were built against.
    let mut s = Solver::new();
    for _ in 0..n {
        s.new_var();
    }
    for c in clauses {
        s.add_clause(c);
    }
    s.solve()
}

#[test]
fn lit_packing_roundtrips() {
    let v = Var(5);
    let p = Lit::positive(v);
    let q = Lit::negative(v);
    assert_eq!(p.var(), v);
    assert!(!p.is_negated());
    assert!(q.is_negated());
    assert_eq!(p.negate(), q);
    assert_eq!(q.negate(), p);
}

#[test]
fn trivial_sat() {
    let mut s = Solver::new();
    let a = s.new_var();
    let b = s.new_var();
    s.add_clause(&[Lit::positive(a), Lit::positive(b)]);
    match s.solve() {
        SatResult::Sat(m) => assert!(m[a.index()] || m[b.index()]),
        other => panic!("expected sat, got {other:?}"),
    }
}

#[test]
fn trivial_unsat() {
    let mut s = Solver::new();
    let a = s.new_var();
    s.add_clause(&[Lit::positive(a)]);
    s.add_clause(&[Lit::negative(a)]);
    assert_eq!(s.solve(), SatResult::Unsat);
}

#[test]
fn empty_clause_is_unsat() {
    let mut s = Solver::new();
    s.new_var();
    s.add_clause(&[]);
    assert_eq!(s.solve(), SatResult::Unsat);
}

#[test]
fn unit_propagation_chain() {
    // a, ¬a∨b, ¬b∨c  ⇒  a,b,c all true.
    let mut s = Solver::new();
    let a = s.new_var();
    let b = s.new_var();
    let c = s.new_var();
    s.add_clause(&[Lit::positive(a)]);
    s.add_clause(&[Lit::negative(a), Lit::positive(b)]);
    s.add_clause(&[Lit::negative(b), Lit::positive(c)]);
    match s.solve() {
        SatResult::Sat(_) => {
            assert!(s.value(a));
            assert!(s.value(b));
            assert!(s.value(c));
        }
        other => panic!("expected sat, got {other:?}"),
    }
}

#[test]
fn pigeonhole_3_into_2_is_unsat() {
    // 3 pigeons, 2 holes: classic unsatisfiable instance.
    let (pigeons, holes) = (3usize, 2usize);
    let mut s = Solver::new();
    let mut x = vec![vec![Var(0); holes]; pigeons];
    for row in x.iter_mut() {
        for slot in row.iter_mut() {
            *slot = s.new_var();
        }
    }
    // Each pigeon occupies at least one hole.
    for row in &x {
        let clause: Vec<Lit> = row.iter().map(|&v| Lit::positive(v)).collect();
        s.add_clause(&clause);
    }
    // No hole holds two pigeons.
    #[allow(clippy::needless_range_loop)]
    for h in 0..holes {
        for i in 0..pigeons {
            for j in (i + 1)..pigeons {
                s.add_clause(&[Lit::negative(x[i][h]), Lit::negative(x[j][h])]);
            }
        }
    }
    assert_eq!(s.solve(), SatResult::Unsat);
}

#[test]
fn random_3sat_matches_brute_force() {
    let mut rng = Rng(0x1234_5678);
    let n = 6usize;
    for _ in 0..400 {
        let m = 5 + rng.below(20) as usize;
        let mut clauses: Vec<Vec<Lit>> = Vec::with_capacity(m);
        for _ in 0..m {
            let mut c = Vec::with_capacity(3);
            while c.len() < 3 {
                let v = Var(rng.below(n as u64) as u32);
                let l = Lit::new(v, rng.below(2) == 1);
                if !c.contains(&l) && !c.contains(&l.negate()) {
                    c.push(l);
                }
            }
            clauses.push(c);
        }
        let expected = brute(n, &clauses);
        match solve_clauses(n, &clauses) {
            SatResult::Sat(model) => {
                assert!(expected, "solver said sat but instance is unsat");
                assert!(satisfies(&clauses, &model), "returned model is not a model");
            }
            SatResult::Unsat => assert!(!expected, "solver said unsat but instance is sat"),
            SatResult::Unknown => panic!("no budget was set; unknown is impossible"),
        }
    }
}

#[test]
fn luby_prefix_is_correct() {
    let expected = [1, 1, 2, 1, 1, 2, 4, 1, 1, 2, 1, 1, 2, 4, 8];
    let got: Vec<u64> = (0..expected.len() as u64).map(luby).collect();
    assert_eq!(got, expected);
}
