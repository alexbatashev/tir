# Spec 02 implementation handoff

This workspace contains an incomplete implementation of `spec-02-fp-and-effects.md`. Do not merge it. The branch is `float_semantics` at `36fb5d68aa9a150b237db1c9b2aedcb31ad899cf`, with all implementation work left uncommitted.

The latest focused RISC-V FP instruction-selection run passes 6 of 9 tests. Three effectful cases still fail because instruction selection cannot cover a demanded `StateResult`. The workspace has not passed the full test, formatting, or lint gates.

## Source material and recovery points

- Spec: `/home/alex/project-plans/tir/floating-semantics/spec-02-fp-and-effects.md`
- Shared contract: `/home/alex/project-plans/tir/floating-semantics/contract.md`
- Effect design: `/home/alex/project-plans/tir/floating-semantics/plan.html#effects`
- Decision trail: `/home/alex/project-plans/tir/_trails/2026-09-12-spec-02-fp-effects.tsv`
- Pre-work base: `36fb5d68aa9a150b237db1c9b2aedcb31ad899cf`
- Safety branch for the earlier rejected implementation: `codex/spec02-rejected-20260912`
- Stashed rejected follow-up changes: `stash@{0}`, named `rejected-spec02-review-fixes-20260912`

Do not restore the stash wholesale. It contains the design the review rejected, including backend-specific behavior in core.

## Required architecture

Keep these constraints when continuing:

- Scalar FP operations belong to the new `core/src/dialects/fp/` dialect. Do not put RISC-V register names, CSR encodings, or target behavior in core.
- `!state<memory>` and `!state<fp.env>` are distinct resource state types. Effects and semantic state accesses identify the resource and field structurally.
- Arithmetic with observable exceptions models the numeric value and flags as one correlated outcome. Rounded symbolic operators return `Pair<Float, bits5>`, observed through `FPValue` and `FPFlags`.
- A machine instruction with effects is one event tile. Its event root is separate from its value and state result ports. Do not synthesize target events from `ResourceEffects` metadata.
- TMDL target behavior produces the target semantic graph. Core proves refinement using target-independent symbolic state fields.
- Dynamic rounding keeps its invalid-mode trap branch until refinement proves that the source domain excludes it.
- `AnyQuiet` and `PreservePayload` are different observations. A rule valid for `AnyQuiet` must not select `PreservePayload` arithmetic.
- Ignore backend register names when deciding whether state can be dropped. Use graph structure and the source operation's declared observations.

These points came from an Astra xhigh architecture review during this run.

## Implemented work

The current diff adds or changes the following major pieces:

- New scalar `fp` dialect under `core/src/dialects/fp/`, including arithmetic, constants, bit/sign operations, comparisons, conversions, and environment operations.
- Removal of the old builtin floating operation module and migration of existing IR, frontend, C API, and backend fixtures to `fp.*` spelling.
- Resource-qualified state types and generalized resource-effect plumbing across parsing, verification, restructuring, calls, and instruction selection.
- Exact FP environment support for rounding modes, flags, traps, save, restore, hold, and update.
- Correlated rounded outcomes in `utils/symbolic`, including `Pair` runtime and e-graph types.
- TMDL behavior analysis that derives guarded FP semantics from formal target behavior.
- Event-result coupling in the PBQP instruction cover through `result_classes` and `Provided` alternatives.
- Target-independent refinement checks for state events, dynamic-rounding guards, and `AnyQuiet` numeric results.
- Explicit PreservePayload observation through a bitcast round trip so PreservePayload graphs cannot collapse to AnyQuiet graphs.
- Value-only target rules for fixed FP instructions when the source ignores FP state.
- New focused interpreter, verifier, round-trip, restructuring, and RISC-V instruction-selection checks.

The diff is broad: 239 files, about 4,325 insertions and 1,772 deletions at handoff time. Much of that is mechanical migration of state and FP operation spelling, but the implementation is not yet ready for review.

## Verified behavior

The latest command was:

```sh
LIT_FILTER='backends/riscv/checks/isel/fp-' cargo test -p tir-lit --test lit
```

Passing checks:

- `fp-clear-flags.tir`
- `fp-preserve-payload-unsupported.tir`
- `fp-get-flags.tir`
- `fp-rounding-modes.tir`
- `fp-dynamic-rounding-flags.tir`
- `fp-round-environment.tir`

Failing checks:

- `fp-state-chain.tir`
- `fp-state-events-distinct.tir`
- `fp-comparison-flags.tir`

All three fail with:

```text
invalid rule set: missing atomic materializer rule for semantic kind StateResult
```

Focused unit tests for symbolic FP state field inference, correlated rounded outcomes, generated dynamic-rounding behavior, PreservePayload observation, and the fixed value-only rule passed earlier in this run. Re-run them because later edits may have changed their assumptions.

## Immediate defect

Start in these paths:

- `core/src/backend/isel/builder.rs`, especially `lower_resource_semantics`
- `core/src/backend/isel/mod.rs`, especially `collect_region_matches`, `root_matches`, and the `region_op_by_root` mapping
- `core/src/backend/isel/cover.rs`, especially `result_classes`, `Provided`, and `completeness_error`
- `tmdl/src/rustgen/instruction_analysis.rs`, where `FpBehaviorEmitter` creates `StateResult`

The generated fixed FP rules have event patterns rooted at `StateResult`, and effectful source operations also lower to `StateResult`. Despite that, no match or provided result reaches the demanded source class in the three failing tests. Determine whether the source event graph differs from the generated target event graph, or whether the region root/value-port mapping loses the event match. Do not add an atomic `StateResult` materializer. `StateResult` represents an event and its result ports, not a standalone instruction.

Useful generated rule names are `fdivdrne`, `fltd`, and `feqd`. Their value-only forms have the `_ignore` suffix. Generated Rust is under `target/debug/build/tir-riscv-*/out/riscv.rs`; treat it as diagnostic output only.

## Known incomplete or suspect areas

1. Conversions are not complete. `core/src/dialects/fp/convert.rs` needs a test-first audit. Rounded conversion semantics must project `FPValue` from the correlated outcome, and effectful conversions need `HasResourceSemantics` with correct flag and state results.
2. Negative refinement tests are missing. Add cases that reject a target trap on a source-valid rounding mode and reject wrong flag or trap behavior.
3. `RuleResourceEffect`, `Rule.resource_effects`, `RuleSpec.resources`, and their TMDL generation appear obsolete after target graphs became authoritative. Remove them if no remaining caller needs them.
4. `StateFieldSchema` currently assigns the whole FP environment a maximum of `0x1fff`. Only the rounding field has the logical `0..=4` restriction. Recheck whether the whole snapshot should have no semantic maximum.
5. `value_observation_fallback` in `utils/symbolic/src/lang/infer.rs` needs a cleanup review. It rebuilds subgraphs with separate memo tables and should preserve shared symbols and types without relying on accidental graph shape.
6. Remove current unused imports in `tmdl/src/rustgen/instruction_analysis.rs` and `core/src/backend/isel/mod.rs` before linting.
7. Audit the full acceptance matrix. Coverage is still incomplete for constant results with flags, signaling NaN arithmetic, conversion edge cases, illegal joins and duplicate writes, branches and loops, unknown calls, trap and memory order, and native CPU execution.
8. Update instruction-selection documentation for the new event-refinement and multi-result tile model.

## Recommended continuation order

1. Reproduce and fix the three `StateResult` cover failures without adding a special-case materializer.
2. Re-run the nine focused RISC-V FP LIT tests.
3. Complete conversions one public test at a time.
4. Add the missing negative refinement and acceptance tests.
5. Remove obsolete rule-resource metadata and compiler warnings.
6. Run focused core interpreter, verifier, round-trip, restructuring, memory, dependency, and backend suites.
7. Run the scalar reference comparison stage.
8. Run the required workspace tests, formatter, and linter.
9. Ask Astra xhigh for the final spec and tidiness review. Fix its findings before declaring completion.

## Required final checks

```sh
cargo test -p tir-adt
cargo test --workspace
cargo xtask fp-check check --stage scalar --reference /tmp/fp-reference.json --output /tmp/fp-stage2.json
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Also run the relevant `tir-unit-tests` property tests and the focused LIT suites named in the spec. No final check above has completed successfully for the current workspace.

## Current warning state

The latest focused run reports unused imports at:

- `tmdl/src/rustgen/instruction_analysis.rs:226`
- `tmdl/src/rustgen/instruction_analysis.rs:284`
- `core/src/backend/isel/mod.rs:28`

`git diff --check` produced no whitespace errors at handoff time.

## Handoff verdict

The current workspace contains useful pieces of the intended design, but it does not satisfy spec 02. Keep it as an implementation branch for selective continuation. Do not mark step 2 complete, merge it, or use it as the accepted base for later floating-semantics steps.
