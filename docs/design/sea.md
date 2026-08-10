# Sea: TIR's core IR

Status: Stage-2 design, revision 2, pending review. Supersedes `gated_ssa.rs`
(deleted in Stage 2). For control flow it generalizes tir-plans
`design-cfg-egraph.md`; that proposal's P1-P3 rewrite content carries over
unchanged as the in-region story.

## 1. What sea is

The IR — a full replacement for the current Context/op storage, not an
optimizer sidecar. It has two layers:

**The substrate: ops, regions, direct manipulation.** Ops are dialect-defined
first-class types — identity is (dialect, name), with attributes, results,
verifiers, interfaces — stored in data-oriented arenas (§4). Ops own regions;
passes walk, create, replace, erase, and move ops and regions imperatively
through a rewriter API, and schedule against concrete op types
(`nest::<FuncOp>`, `PassTarget`) exactly as today. A dialect-conversion
framework (§5) does progressive lowering between dialects with type
conversion. This layer is what gpu/openmp-style transforms, outlining,
inlining, and every future pass we have not imagined program against. Nothing
here requires the e-graph.

**The optimization layer: gated graph regions.** A region comes in one of two
kinds, declared by its owning op:

- **CFG region**: ordered blocks with block arguments and terminators.
  Unstructured by design — it implements none of the structured interfaces,
  and that is a feature, not a gap. Machine code after destruction lives
  here; frontends and hand-written dialects may produce it freely. It is
  manipulated imperatively only.
- **Graph region**: a single implicit block whose pure value ops are stored
  hash-consed (the region *is* an e-graph), with memory and other effects
  threaded as explicit state values. Equality saturation, cost extraction,
  and PBQP covering operate here. Congruence closure over this form is
  Alpern-Wegman-Zadeck value numbering; hash-consing is Havlak's thinning.

Raising (CFG → graph, restructuring) and destruction (graph → CFG, at
emission or on demand) convert between the kinds. The mid-level pipeline
keeps function bodies in graph form; nothing forces it — a body that stays
CFG simply doesn't benefit from the optimizer.

## 2. Structured-op interfaces

The optimizer binds exclusively to interfaces; it never names a concrete op.
Names follow the existing TIR convention (`LoopLike`, `MemoryRead`); the
theory literature's γ/θ/λ appear only as parenthetical shorthand here. Each
interface carries *laws*: proof obligations an implementing op discharges
through the existing oracle machinery, the same way guarded isel rules prove
their relaxations. An op that cannot discharge the laws cannot claim the
interface.

CFG regions implement none of these. An op whose regions are CFG is opaque to
gate reasoning until raised.

### 2.1 `Conditional` (γ)

An op with n ≥ 2 disjoint graph sub-regions, a deciding operand, k results;
each region yields k values.

- laws (SMT `ite` obligations): C1 selection — `decide = i ⊢ result_j =
  region_i.yield_j`; C2 totality — `decide` in range or a default region
  declared; C3 speculation boundary — a region is speculatable iff pure and
  non-trapping; only then may rewrites collapse the op into a value-level
  `If` term (cmov/csel candidacy).
- canonical impl: `scf.if`. A switch, a GPU divergence construct, or an
  openmp conditional are other impls with their own semantics.

### 2.2 `LoopLike` (θ) — generalized from today's interface

One body graph region, n loop-carried ports (today's interface models one;
this is the n-ary generalization), region-invariant inputs, a continuation
condition yielded by the body, and n final results.

- laws: L1 carried/final distinction — `final_j` is never congruent to
  `carried_j` (equal only on the last iteration; structurally distinct
  classes, no law may merge them — this is Stage 1's `Eta`, made native);
  L2 invariance — `next_j = carried_j ⊢ final_j = init_j`, an induction
  obligation (base at init, step through next); L3 no unrolling under
  saturation — a rewrite nesting the body inside itself is rejected at
  ruleset load. Unrolling is a legal *structural* transform on the substrate
  (body cloning), just not a saturation rule.
- canonical impls: `scf.for`, `scf.while`. An `omp.wsloop` or a hardware
  pipelined-loop op implement the same interface with their own semantics.

### 2.3 `IsolatedFromAbove` and `FunctionLike` (λ)

- `IsolatedFromAbove`: the op's regions reference nothing from enclosing
  regions except through declared operands. Today FuncOp's implicit
  property; made checkable. The unit of region extraction (outlining,
  gpu.launch capture) and of parallel compilation.
- `FunctionLike`: `IsolatedFromAbove` + symbol + signature + call contract.
  `builtin.func` is one impl; a GPU kernel, an openmp outlined body, or a
  coroutine are others, each answering the inliner's legality query
  differently (a kernel refuses inlining across the host/device boundary).
- law F1: a call site exhibits exactly the callee's declared effects (its
  state ports).

### 2.4 State threading (memory as values)

Memory/ordering effects are values of the `!token`-style state type: a load
takes a state, a store takes and produces one. `MemoryRead`/`MemoryWrite`
grow state accessors; `Conditional`/`LoopLike` regions that touch memory
carry state through their ports like any value (replacing `TokenScope`).

- granularity: one chain per non-escaping alloca + one conservative chain;
  alias analysis later refines chains without changing the contract.
- laws (SMT obligations; this kernel IS mem2reg once memory is threaded):
  S1 forwarding `load(store(s,p,v),p) = v`; S2 dead store
  `store(store(s,p,_),p,v) = store(s,p,v)`; S3 disjoint commutation.
- isel consequence: memory e-nodes keyed by state operand; `SemPayload::
  Opaque` serials die; load CSE becomes a legal congruence.

## 3. Direct manipulation and future passes

Explicitly supported, on the substrate, without touching the e-graph:

- **Dialect conversion** (§5): pattern-based progressive lowering with type
  converters — cir → scf, sea → machine dialects, future x → y.
- **Region surgery**: wrap (put a loop body under `omp.parallel`), outline
  (extract an `IsolatedFromAbove` op → `gpu.func` + launch), clone
  (unrolling, peeling, inlining via `FunctionLike`), splice.
- **Pass targeting**: concrete op types remain the scheduling unit.
- Op creation/mutation in CFG regions is positional and in-place. In graph
  regions ops are hash-consed terms, so "mutation" is create-new +
  replace-uses (functional update) — the rewriter API makes both feel the
  same; only the identity semantics differ.

## 4. Storage (data-oriented)

- One arena per `IsolatedFromAbove` tree; single-writer; parallelism across
  isolated ops. No `Arc`, no locks on the compiled-code path.
- SoA columns: interned op-type id (u32), payloads (fixed-size,
  niche-packed), operands/results/regions/blocks as CSR segments (empty for
  pure terms); dense u32 ids; 64-byte-aligned chunks; kind-major scans.
- Graph regions add the e-graph layer over the same columns: flat union-find
  (path halving), open-addressing hash-cons indexing into columns,
  congruence rebuild by sorted runs, scopes as watermark + undo log
  (O(delta) push/pop).
- Interface dispatch: dense per-op-type vtable arrays resolved at
  registration; no hash lookups on hot paths.
- Determinism by construction: dense append-only ids, no bare-HashMap
  iteration where order can leak.

## 5. Dialect conversion framework

Generalizes today's one-off lowerings (`scf_to_cfg`, cir lowering, isel
emission plumbing) into one mechanism: a conversion target (legal/illegal op
sets per dialect), typed rewrite patterns (imperative, on the substrate), and
type conversion with materializations. Destruction (graph → CFG) and raising
(CFG → graph restructuring, Bahmann-Reissmann when goto arrives) are
conversions in this framework, not special passes. Machine dialects are
ordinary conversion targets.

## 6. What binds where

| machinery | binds to |
|---|---|
| builder / raising | `Conditional`/`LoopLike`/state interfaces of source ops |
| saturation (axioms, PDL, target rules) | flat terms within one graph region; ports are leaves |
| interface laws (C*, L*, S*, F1) | verified per impl at registration/CI via the oracle |
| extraction (cost / PBQP) | terms + per-gate `reify` alternative |
| destruction / conversions | the conversion framework (§5) |
| imperative passes, gpu/openmp, inliner | substrate rewriter + interfaces; no e-graph knowledge |
| pass manager | concrete op types, as today |

## 7. Stage 2 scope (in order)

1. Interface definitions + law obligations wired to the oracle: n-ary
   `LoopLike`, `Conditional`, `IsolatedFromAbove`/`FunctionLike`, state
   accessors on `MemoryRead`/`MemoryWrite`.
2. fcc lowers all control flow to scf (short-circuit included); mid-end sees
   no raw `cond_br`; `gated_ssa.rs` deleted.
3. `core/src/sea`: arenas, columns, region kinds, graph-region e-graph,
   builder from scf/cir, state threading.
4. isel substrate swap: TMDL rules + PBQP over sea graph regions; Opaque
   serials die; old path behind a flag until byte-equal or reviewed-better.

Out of scope for Stage 2: the conversion framework's full generality (only
what the substrate swap needs), EGVN (Stage 3), authority flip (Stage 4),
alias classes, additional `Conditional`/`LoopLike` impls beyond scf.
