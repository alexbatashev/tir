//! Branch-direction predictors — the first swappable microarchitecture policy of
//! the dynamic engine. A predictor sees a conditional branch (its address and
//! resolved target) and guesses taken/not-taken; the timing model charges a
//! misprediction penalty when the guess is wrong. New predictors are added by
//! implementing [`BranchPredictor`] in Rust — no TMDL involved.

/// A conditional-branch direction predictor.
pub trait BranchPredictor {
    /// Predict whether the branch at `pc` with destination `target` is taken.
    fn predict(&mut self, pc: u64, target: u64) -> bool;

    /// Update the predictor with the resolved outcome. Static predictors ignore it.
    fn update(&mut self, pc: u64, target: u64, taken: bool) {
        let _ = (pc, target, taken);
    }

    fn name(&self) -> &'static str;
}

/// Always predicts not-taken — the simplest possible static predictor and a useful
/// baseline. Correct only on fall-through branches; mispredicts every taken branch
/// (so it is poor on loops).
#[derive(Debug, Default, Clone, Copy)]
pub struct AlwaysNotTaken;

impl BranchPredictor for AlwaysNotTaken {
    fn predict(&mut self, _pc: u64, _target: u64) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "always-not-taken"
    }
}

/// Backward-taken / forward-not-taken (BTFN): the classic static loop heuristic. A
/// branch to an earlier address is a loop back-edge and predicted taken; a forward
/// branch (an `if`-style skip) is predicted not-taken. Mispredicts only the loop
/// exit, so it is far better than [`AlwaysNotTaken`] on loops.
#[derive(Debug, Default, Clone, Copy)]
pub struct BackwardTaken;

impl BranchPredictor for BackwardTaken {
    fn predict(&mut self, pc: u64, target: u64) -> bool {
        target < pc
    }

    fn name(&self) -> &'static str {
        "btfn"
    }
}

/// Construct a predictor by name (`not-taken` / `btfn`), for CLI selection.
pub fn by_name(name: &str) -> Option<Box<dyn BranchPredictor>> {
    match name {
        "not-taken" | "always-not-taken" => Some(Box::new(AlwaysNotTaken)),
        "btfn" | "backward-taken" => Some(Box::new(BackwardTaken)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_not_taken_never_predicts_taken() {
        let mut p = AlwaysNotTaken;
        assert!(!p.predict(0x100, 0x80)); // backward
        assert!(!p.predict(0x100, 0x180)); // forward
    }

    #[test]
    fn btfn_predicts_backward_taken_forward_not_taken() {
        let mut p = BackwardTaken;
        // Backward branch (loop back-edge) → taken.
        assert!(p.predict(0x100, 0x80));
        assert!(p.predict(0x80000010, 0x80000004));
        // Forward branch (skip) → not taken.
        assert!(!p.predict(0x100, 0x180));
        // A branch to itself is not "backward".
        assert!(!p.predict(0x100, 0x100));
    }

    #[test]
    fn by_name_selects_predictors() {
        assert_eq!(by_name("not-taken").unwrap().name(), "always-not-taken");
        assert_eq!(by_name("btfn").unwrap().name(), "btfn");
        assert!(by_name("nope").is_none());
    }
}
