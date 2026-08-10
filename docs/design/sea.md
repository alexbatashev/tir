# Sea: TIR's core IR

Status: Stage-2 design, revision 3, pending review. Supersedes `gated_ssa.rs`
(deleted in Stage 2). Control-flow rewrite content of tir-plans
`design-cfg-egraph.md` (P1-P3) carries over as the e-graph-view story.

## 1. Architecture: one core, many views

Two levels, in the spirit of rust-analyzer's red-green trees:

**The core (green): a canonical RVSDG.** One authoritative, heavier,
data-oriented representation: a structured region tree of dialect-defined ops
with gated control flow (`Conditional`/`LoopLike` interfaces, §2), memory and
effects threaded as state values, and explicit ports. Persistent by structural
sharing: an edit builds new nodes along the changed spine; untouched subtrees
keep their node identity. Mutation goes through the core's own API — create,
replace-subtree, wrap, clone, splice — and is what every imperative pass
(dialect conversion, gpu/openmp restructuring, outlining, inlining) programs
against. Single writer per `IsolatedFromAbove` tree; parallelism across them.

**Views (red): shims over the core, cached in Context.** A view renders or
indexes the core for one purpose and declares its mutability:

| view | kind | purpose |
|---|---|---|
| e-graph | mutating-by-commit | equality saturation, cost/PBQP extraction |
| pattern rewriter | mutating-by-commit | dialect conversion, peephole/imperative rewrites |
| dominance / def-use / liveness | read-only | classic analyses |
| CFG rendering | read-only → conversion | destruction to machine dialects |
| textual form | read-only | printing, diffing, tests |

The view catalog is open — future passes add views without touching the core.

**The sync contract** (the load-bearing rule):
- Every core node carries a version; a mutation bumps versions along the
  edited spine only.
- A view is keyed by (node id, version). Untouched subtrees keep identity, so
  a view's cached content for them stays valid — invalidation is
  proportional to the edit, not the program.
- A *read-only* view never writes the core. A *mutating* view stages changes
  privately and lands them in one atomic `replace-subtree` commit on the
  core; the single-writer rule makes commits race-free. Between commits the
  view may diverge from the core arbitrarily (the e-graph mid-saturation is
  not IR — only its extracted commit is).
- Nothing else may hold the core hostage: a pass that never asks for the
  e-graph never pays for it. The e-graph is a cache, not a gatekeeper.

This buys, beyond separation of concerns: incremental recompilation (cached
analyses — and cached selection — keyed by subtree identity survive edits
elsewhere), speculative transforms (build a view over a candidate subtree,
measure, commit or drop), and cheap parallelism (immutable shared green
nodes).

**Unstructured control flow** lives at the boundaries, not in the core: the
frontend raises everything to structured form before entry (fcc lowers all CF
to scf; goto later via Bahmann-Reissmann restructuring), and machine-level
CFG exists after destruction in the machine dialects as today. The core is
structured, period — that is what makes the gate interfaces total over it.

## 2. Structured-op interfaces (unchanged from rev 2 except attachment)

Ops are dialect-defined first-class types — (dialect, name), attributes,
verifiers, interfaces; pass scheduling to concrete op types is preserved. The
optimizer and every view bind to interfaces, never to concrete ops. Names
follow existing TIR convention; γ/θ/λ are parenthetical shorthand only.

Interfaces carry *laws* — proof obligations discharged per implementing op
through the existing oracle machinery. No laws, no interface.

### 2.1 `Conditional` (γ)
n ≥ 2 disjoint sub-regions, deciding operand, k results.
Laws: C1 selection (`decide = i ⊢ result_j = region_i.yield_j`); C2 totality;
C3 speculation boundary (region speculatable iff pure and non-trapping; only
then may the e-graph view collapse it to a value-level `If` term).
Canonical impl `scf.if`; switches, divergence constructs, openmp conditionals
are other impls.

### 2.2 `LoopLike` (θ) — n-ary generalization of today's interface
One body region; n carried ports (init / carried / next / final), invariant
inputs, a yielded continuation condition.
Laws: L1 carried/final distinction (`final_j` never congruent to `carried_j`
— Stage 1's `Eta`, made native structure); L2 invariance (`next_j =
carried_j ⊢ final_j = init_j`, induction obligation); L3 unrolling is banned
as a saturation rule (non-terminating) and legal as a structural clone on the
core.
Canonical impls `scf.for`/`scf.while`; `omp.wsloop`, pipelined hardware loops
are other impls.

### 2.3 `IsolatedFromAbove` and `FunctionLike` (λ)
`IsolatedFromAbove`: regions reference the outside only through declared
operands — the unit of outlining, offload capture, arena ownership, and
parallel compilation. `FunctionLike`: + symbol, signature, call contract;
`builtin.func` is one impl; GPU kernels, outlined openmp bodies, coroutines
answer inlining/legality queries their own way.
Law F1: a call site exhibits exactly the callee's declared effects (state
ports).

### 2.4 State threading
Loads take a state value; stores take and produce one; `MemoryRead`/
`MemoryWrite` grow state accessors; gated ops carry state through ports
(replaces `TokenScope`). One chain per non-escaping alloca + one conservative
chain; alias analysis refines later without contract change.
Laws: S1 `load(store(s,p,v),p) = v`; S2 `store(store(s,p,_),p,v) =
store(s,p,v)`; S3 disjoint commutation. This kernel IS mem2reg once memory is
threaded. In the e-graph view, memory nodes key on their state operand —
`SemPayload::Opaque` serials die; load CSE becomes a legal congruence.

## 3. Core storage (data-oriented)

- Arena per `IsolatedFromAbove` tree; append-only columns; structural sharing
  by construction (new spine nodes reference old subtree ids); a periodic
  compaction reclaims dead versions.
- SoA: interned op-type id (u32), fixed-size niche-packed payloads,
  operands/results/regions/ports as CSR segments; dense u32 ids;
  64-byte-aligned chunks; kind-major scans.
- Version stamps per node (u32, bumped on spine); (id, version) is the
  view-cache key.
- Interface dispatch via dense per-op-type vtable arrays resolved at
  registration.
- Determinism by construction: dense append-only ids; no bare-HashMap
  iteration where order can leak.

## 4. The e-graph view

Built lazily per region (or per isolated tree) from the core: pure terms
hash-cons on entry; gated ops enter through their interfaces (a
`Conditional` with speculatable regions contributes `If` terms; a `LoopLike`
contributes the carried-value cycle via placeholder+union; final ports stay
distinct per L1). Saturation with axioms/PDL/target rules, scoped assumptions
as watermark+undo, then either cost extraction (canonicalizer/EGVN mode) or
PBQP covering (isel mode). The chosen terms are materialized as a staged
subtree and committed atomically. Cache: the view persists in Context keyed
by (region id, version); an edit elsewhere leaves it warm.

## 5. Conversion framework

One mechanism for progressive lowering on the core via the pattern-rewriter
view: conversion targets (legal/illegal op sets), typed patterns, type
converters with materializations. Raising (boundary CFG → core) and
destruction (core → machine dialects) are conversions, not special passes.

## 6. Stage 2 scope (in order)

1. Interfaces + laws wired to the oracle: n-ary `LoopLike`, `Conditional`,
   `IsolatedFromAbove`/`FunctionLike`, state accessors.
2. fcc lowers all control flow to scf; `gated_ssa.rs` deleted.
3. `core/src/sea`: green arenas/columns/versions, core mutation API, the
   e-graph view with the sync contract.
4. isel substrate swap: TMDL rules + PBQP as the e-graph view's isel mode;
   old path behind a flag until byte-equal or reviewed-better.

Out of scope: full conversion-framework generality (only what the swap
needs), EGVN (Stage 3), authority flip of Context storage (Stage 4), alias
classes, non-scf gate impls.
