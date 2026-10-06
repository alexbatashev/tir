//! Per-round saturation counters: how much of the graph each round searched and
//! how much of that search was already-applied work. Printed as `tir-sat:` lines
//! on stderr under `TIR_TIME_PASSES`, alongside the pass-timing table.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::Stats;
use crate::rule::Client;
use crate::saturate::Delta;

/// Mirrors the core pass timer's switch; this crate cannot see it.
pub fn enabled() -> bool {
    static FROM_ENV: OnceLock<bool> = OnceLock::new();
    *FROM_ENV.get_or_init(|| std::env::var_os("TIR_TIME_PASSES").is_some_and(|value| value != "0"))
}

fn rule_stats_enabled() -> bool {
    static FROM_ENV: OnceLock<bool> = OnceLock::new();
    *FROM_ENV.get_or_init(|| std::env::var_os("TIR_RULE_STATS").is_some_and(|value| value != "0"))
}

#[derive(Default, Clone, Copy)]
struct Round {
    changed: usize,
    delta: usize,
    searched: usize,
    found: usize,
    noop: usize,
    merged: usize,
    repairs: usize,
}

thread_local! {
    static RULES: RefCell<BTreeMap<String, RuleCounts>> = const { RefCell::new(BTreeMap::new()) };
    static ROUNDS: RefCell<Vec<Round>> = const { RefCell::new(Vec::new()) };
    static ELAPSED: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    static EXTRACTED: Cell<(usize, Duration)> = const { Cell::new((0, Duration::ZERO)) };
}

#[derive(Default)]
struct RuleCounts {
    applications: usize,
    changed: usize,
}

pub(super) fn register_rules<'a>(names: impl IntoIterator<Item = &'a str>) {
    if rule_stats_enabled() {
        RULES.with(|counts| {
            let mut counts = counts.borrow_mut();
            for name in names {
                counts.entry(name.to_string()).or_default();
            }
        });
    }
}

/// Whether a match's application is counted at all: [`RoundStats::apply`]
/// and [`apply_rule`] do nothing otherwise.
pub(super) fn counting() -> bool {
    enabled() || rule_stats_enabled()
}

pub(super) fn apply_rule<C, const S: usize, X: Client<C, S>>(
    name: &str,
    client: &mut X,
    apply: impl FnOnce(&mut X),
) {
    if !rule_stats_enabled() {
        return apply(client);
    }
    let before = client.db().stats();
    apply(client);
    let after = client.db().stats();
    RULES.with(|counts| {
        let mut counts = counts.borrow_mut();
        let count = counts.get_mut(name).expect("registered saturation rule");
        count.applications += 1;
        count.changed += usize::from(
            (after.merges, after.adds, after.raises) != (before.merges, before.adds, before.raises),
        );
    });
}

/// Record one extraction: it costs a pass over every id, and instcombine runs
/// one per region, so the count is as interesting as the time.
pub(super) fn count_extract(elapsed: Duration) {
    if enabled() {
        EXTRACTED.with(|total| {
            let (runs, time) = total.get();
            total.set((runs + 1, time + elapsed));
        });
    }
}

/// Wall time inside a saturation driver. The pass timer measures a whole pass —
/// seeding, saturation, extraction and rewiring — so the saturation gates of the
/// plan need this narrower number to be judged on.
pub struct Timer(Option<Instant>);

impl Timer {
    pub fn start() -> Self {
        Self(enabled().then(Instant::now))
    }

    pub fn finish(self) {
        if let Some(started) = self.0 {
            ELAPSED.with(|total| total.set(total.get() + started.elapsed()));
        }
    }
}

/// One saturation round's counters, or nothing when telemetry is off — every
/// method is then a plain pass-through. The database counts its own work, so a
/// round is the difference across it: live ids and the client's size cannot
/// answer this, since a match may equate two ids and mint two more.
pub struct RoundStats(Option<(Round, Stats)>);

impl RoundStats {
    /// Open a round searching against `delta` (`None` on the first round, which
    /// searches everything).
    pub fn start<G>(stats: Stats, delta: Option<&Delta<G>>) -> Self {
        if !enabled() {
            return Self(None);
        }
        Self(Some((
            Round {
                changed: delta.map_or(usize::MAX, Delta::len),
                ..Round::default()
            },
            stats,
        )))
    }

    /// One rule's root set for this round, drawn from `delta`'s frontier.
    pub fn searched<G>(&mut self, roots: usize, delta: Option<&Delta<G>>) {
        let Some((round, _)) = &mut self.0 else {
            return;
        };
        round.searched += roots;
        round.delta = round.delta.max(delta.map_or(0, Delta::frontier));
    }

    /// Apply one match, counting it as a no-op if it equated nothing, minted
    /// nothing and raised nothing — the match was already there, as re-finding
    /// an old one is.
    pub fn apply<C, const S: usize, X: Client<C, S>>(
        &mut self,
        client: &mut X,
        apply: impl FnOnce(&mut X),
    ) {
        let Some((round, _)) = &mut self.0 else {
            return apply(client);
        };
        round.found += 1;
        let before = client.db().stats();
        apply(client);
        let after = client.db().stats();
        round.noop += usize::from(
            (after.merges, after.adds, after.raises) == (before.merges, before.adds, before.raises),
        );
    }

    /// Close the round; call after the rebuild, whose merges and repairs it counts.
    pub fn finish(self, now: Stats) {
        let Some((mut round, base)) = self.0 else {
            return;
        };
        round.merged = now.merges - base.merges;
        round.repairs = now.repairs - base.repairs;
        ROUNDS.with(|rounds| rounds.borrow_mut().push(round));
    }
}

/// Print and reset the telemetry recorded since the last call.
pub fn report_saturation(pass: &str) {
    if rule_stats_enabled() {
        RULES.with(|counts| {
            for (name, count) in std::mem::take(&mut *counts.borrow_mut()) {
                eprintln!(
                    "tir-rule: pass={pass} rule={name} applications={} changed={} noop={}",
                    count.applications,
                    count.changed,
                    count.applications - count.changed,
                );
            }
        });
    }
    if !enabled() {
        return;
    }
    let extracts = EXTRACTED.replace((0, Duration::ZERO));
    let rounds = ROUNDS.with(|rounds| std::mem::take(&mut *rounds.borrow_mut()));
    if rounds.is_empty() {
        return;
    }
    // `changed` is the previous round's change log; the first round of each
    // saturation has none and searches everything, printed as `all`.
    let column = |pick: fn(&Round) -> usize| {
        let cells: Vec<String> = rounds
            .iter()
            .map(|round| match pick(round) {
                usize::MAX => "all".to_string(),
                value => value.to_string(),
            })
            .collect();
        cells.join(",")
    };
    eprintln!(
        "tir-sat: pass={pass} rounds={} saturate_ms={:.3} extracts={} extract_ms={:.3} \
         changed=[{}] delta=[{}] searched=[{}] found=[{}] noop=[{}] merged=[{}] repairs=[{}]",
        rounds.len(),
        ELAPSED.replace(Duration::ZERO).as_secs_f64() * 1e3,
        extracts.0,
        extracts.1.as_secs_f64() * 1e3,
        column(|r| r.changed),
        column(|r| r.delta),
        column(|r| r.searched),
        column(|r| r.found),
        column(|r| r.noop),
        column(|r| r.merged),
        column(|r| r.repairs),
    );
}
