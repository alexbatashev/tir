//! Choosing among candidate edits of one overlay.
//!
//! Each candidate is built in a fork of the overlay and scored there; the
//! cheapest fork is adopted, the others are dropped. Nothing is committed and
//! the base is never touched, so a rejected candidate leaves no id behind and
//! moves no arena count. A tie goes to the lower index, so the choice depends
//! on the candidates' order and nothing else.

use crate::{Context, PassError};

/// Build every candidate of `candidates` in its own fork of `context`, score
/// each fork with `score` (lower is better), and adopt the cheapest one that
/// beats the overlay as it stands. `build` answers `false` for a candidate
/// that does not apply, which is then skipped. Returns the index adopted.
pub fn choose<C>(
    context: &Context,
    candidates: &[C],
    mut build: impl FnMut(&Context, &C) -> Result<bool, PassError>,
    mut score: impl FnMut(&Context) -> i64,
) -> Result<Option<usize>, PassError> {
    let mut best: Option<(i64, usize, Context)> = None;
    let standing = score(context);
    for (index, candidate) in candidates.iter().enumerate() {
        let fork = context.fork();
        if !build(&fork, candidate)? {
            continue;
        }
        let cost = score(&fork);
        if cost < standing && best.as_ref().is_none_or(|(lowest, ..)| cost < *lowest) {
            best = Some((cost, index, fork));
        }
    }
    Ok(best.map(|(_, index, fork)| {
        context.adopt(fork);
        index
    }))
}
