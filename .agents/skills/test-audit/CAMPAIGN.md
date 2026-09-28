# Test-pruning campaign

Campaign mode prunes one subsystem's tests in one PR, such as FCC, a backend,
or one core IR area. The value bar, retention bar, candidate evidence, and
validation in [SKILL.md](SKILL.md) apply to every lane. This file adds the
order of work and the lessons of a full campaign. Each step ends on its
completion criterion; do not start the next step early.

## 1. Baseline

Record the subsystem's test and support line counts and every test file's
pass/fail state at a pinned `master` SHA. Keep baseline failures in their own
list. Reproduce them before deciding whether they are product bugs or stale
tests. Record unavailable infrastructure separately from pass/fail results.

Done when every in-scope test file has a recorded baseline result.

## 2. Lanes and inventory

Split the tests into lanes along production owner boundaries, not file
prefixes. For FCC these may include parsing, semantic analysis, IR generation,
and execution. Include the subsystem's cases in `utils/unit-tests`, LIT suites,
formal verification, torture tests, and runtime or benchmark validators where
they protect correctness.

Done when every test file and validation scenario the subsystem owns belongs to
exactly one lane.

## 3. Read-only ledger per lane

Give each lane to a fresh-context read-only agent. Run at most three subagents
at once, reuse slots for later lanes, and keep shared logs with the main agent.
The agent reads every assigned test in full, including parameter tables. It
also reads the production owners and their entry points, callers, history, and
CI routing. Each test declaration goes into a written **ledger** with one mark.
A Rust test or LIT check file is one entry unless its table rows, `RUN:` lines,
or check prefixes need different marks; then record those separately.

- `R`: retain, naming the contract and the bug it catches; a retained test that
  only moves to a better-named file stays `R` with the move noted;
- `F`: retain the contract but repair the assertion, such as a vacuous negative
  that passes when only one of several items is missing;
- `C`: consolidate, naming the owner that absorbs the assertion first: a sibling
  table case, a stronger boundary suite, or the shared owner in another package;
- `D`: delete, naming the proof that remains, or why no contract exists.

Judge a test by its assertions, not its name. A test named for dead-store
elimination must check that behavior, not merely successful compilation.

Done when every declaration in the lane has a mark and an evidence line.

## 4. Layer plan per lane

Treat the per-test ledger as input, not as the edit list. A second read-only
pass, starting from the ledger, looks for the redundant layer. For example,
several Rust tests may rebuild the same IR and compare printed output already
covered by a LIT suite. Name the keeper suite for each contract. Prefer the
real compiler tool or public API over mocked compiler stages. Preserve
stage-specific checks when they detect a distinct defect. Correct any ledger
errors this pass finds.

Done when each lane plan names its retired files, its keeper per contract, the
assertions to carry into keepers, and the test-only production seams unlocked.

## 5. Cutover

Edit lane by lane. Serialize changes to shared harnesses and support files
through one owner. With each lane, remove the test-only production seams it
unlocks: injection parameters, getters, reset exports, and indirection layers.
Check moved suites against `test_suite.toml`, Rust test-module registration,
and CI routing. Do not bless torture failures or regenerate snapshots merely to
accept a regression. Put durable test-ownership rules drawn from mistakes the
campaign actually found in the applicable existing `AGENTS.md`. Creating new
documentation requires authorization under the project rules.

Done when every lane plan is applied and each lane's keepers pass.

## 6. Preservation review

Before claiming completion, have independent reviewers compare deleted coverage
against the keepers, one reviewer per boundary group. They look for contracts
that lost their only proof. They also look for new assertions that cannot fail,
such as a rejection row the production code never reaches. Include LIT
discovery, negative checks, and accidentally skipped solver or execution cases
in this review.

For each restored contract, make one deliberate **mutation** of the production
owner and confirm the keeper goes red. Use an isolated checkout for mutations
and restore its source byte for byte.

Done when every reported gap is restored or rejected with source evidence, and
every restored contract has a caught mutation.
