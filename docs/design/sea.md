# Sea: the gated value+state graph

Status: Stage-2 design, pending review. Companion to the approved redesign plan;
supersedes `gated_ssa.rs` (deleted in this stage) and, for control flow,
generalizes the flat-gate proposal in tir-plans `design-cfg-egraph.md` — its
P1-P3 rewrite content carries over unchanged as the intra-region story.

## 1. What sea is

The compiler's working representation between the frontend dialects and machine
emission: every op stored hash-consed in a region-hierarchical e-graph, memory
and other effects threaded as explicit state values. Congruence closure over
this graph is Alpern-Wegman-Zadeck value numbering; hash-consing is Havlak's
thinning; saturation, cost extraction, and PBQP covering are the optimizer and
the instruction selector on the same object.

Sea changes the storage and computational model only. The extensibility model
is untouched: ops are dialect-defined first-class types — identity is
(dialect, name), with attributes, verifiers, and interfaces — and pass
scheduling to concrete op types (`nest::<FuncOp>`, `PassTarget`) works exactly
as today. Nothing in core or in the optimizer names a concrete op.

## 2. The interface contracts

The optimizer binds exclusively to interfaces. Each contract below carries
*laws*: proof obligations an implementing op discharges through the existing
oracle machinery (SmtOracle / induction obligations), the same way guarded
rules prove their relaxations today. An op that cannot discharge the laws
cannot claim the interface — extensibility without escape hatches.

### 2.1 `Gamma` (decision)

An op with n ≥ 2 disjoint sub-regions, a deciding value operand, and k results;
each region yields k values through its terminator.

- ports: `decide: Value`, regions `r_0..r_{n-1}`, results `y_0..y_{k-1}`
- semantics: `y_j = region_{decide}(inputs).yield_j`; exactly one region's
  effects occur.
- laws (per implementing op, SMT `ite` obligations):
  G1 selection: `decide = i ⊢ y_j = r_i.yield_j`
  G2 totality: `decide` is always in range (or the op declares a default
  region).
  G3 speculation boundary: a region is *speculatable* iff pure and non-trapping;
  only then may rewrites collapse the Gamma into a value-level `If` term.
- canonical impl: `scf.if` (n = 2). A future `switch` is just another impl.

### 2.2 `Theta` (loop)

An op with one body region and k loop-carried ports (plus any number of
region-invariant inputs).

- ports: `init_0..k`, body arguments `carried_0..k` (the μ role), body yields
  `next_0..k`, results `final_0..k` (the η role), and a continuation value the
  body yields (`continue: i1`).
- semantics: `carried^0 = init`; `carried^{t+1} = next(carried^t)` while
  `continue(carried^t)`; `final = carried^T` at the first `t = T` where
  `continue` is false.
- laws:
  T1 μ/η distinction: `final_j` is never congruent to `carried_j` (they are
  equal only at `t = T`) — structurally enforced: distinct classes by
  construction, no law can merge them.
  T2 invariance: `next_j = carried_j ⊢ final_j = init_j`'s general form
  `theta(x, x) = x` — induction obligation (base at `init`, step through
  `next`), the design-cfg-egraph P2 obligation kind.
  T3 no unrolling under saturation: any rewrite whose RHS nests the op's own
  body is rejected at ruleset load (non-terminating).
- canonical impls: `scf.for`, `scf.while`. Requires the n-ary carrying
  generalization (today `LoopLike` models one carried value and fcc builds
  `scf.while` with zero iter-args).

### 2.3 `Isolated` + `Function` (callable)

- `Isolated`: the region references nothing from enclosing regions except
  through declared ports (today's implicit FuncOp property, made a checkable
  interface). Required for region extraction (outlining, offload) and for
  per-region parallel compilation.
- `Function`: `Isolated` + a symbol, an ABI reference, and a call contract
  (argument/result ports typed against a signature). Canonical impl:
  `builtin.func`. A GPU kernel or coroutine is a different op implementing the
  same interface with different call semantics; the inliner (a λ-copy region
  rewrite) works over the interface and asks the impl whether inlining is
  legal (e.g. a kernel says no across the host/device boundary).
- law F1: calls compose — the callee's declared effects (state ports) are the
  only effects a call site exhibits.

### 2.4 `State` (effects as values)

Memory and ordering effects are ordinary values of the existing `!token`-style
state type, threaded: a load takes a state, a store takes and produces one.
`MemoryRead`/`MemoryWrite` grow state accessors; Gamma/Theta carry state
through their ports like any other value (a region that touches memory has a
state port — this replaces `TokenScope`).

- granularity: one chain per non-escaping alloca + one conservative chain for
  everything else. Alias analysis later refines chains; the contract doesn't
  change.
- laws (the mem2reg-as-rewrites kernel, SMT obligations over the state
  algebra):
  S1 forwarding: `load(store(s, p, v), p) = v`
  S2 dead store: `store(store(s, p, _), p, v) = store(s, p, v)`
  S3 commutation: `p ≠ q ⊢` loads/stores on `p` commute past stores on `q`
  (chain-disjointness is the trivial case).
- isel consequence: memory e-nodes are keyed by their state operand, deleting
  `SemPayload::Opaque` serials; loads CSE exactly when their state and address
  classes agree — a legal congruence instead of a forbidden one.

## 3. Storage (data-oriented)

- One arena per `Isolated` region tree; single-writer; parallelism across
  isolated regions. No `Arc`, no locks in the compiled-code path.
- Ops in SoA columns: `op_type: Vec<u32>` (interned (dialect, name) id),
  `payload`, children/ports in shared CSR arrays; dense u32 ids throughout;
  64-byte-aligned column chunks. Pure fixed-arity value terms and
  region-carrying ops live in the same columns — region lists are just another
  CSR segment, empty for pure terms.
- E-graph native: union-find as flat `Vec<u32>` (path halving), open-addressing
  hash-cons over column data, congruence rebuild by sorted runs.
- Scopes and speculative rewrites: column watermark + undo log, O(delta)
  push/pop.
- Interface dispatch: per-op-type vtable index resolved at registration into
  dense arrays — no hash lookups on hot paths.
- Determinism by construction: dense append-only ids give stable traversal;
  no bare-HashMap iteration anywhere order can leak.

## 4. What binds where

| machinery | binds to |
|---|---|
| builder (scf/cir → sea) | Gamma/Theta/State interfaces of the source ops |
| saturation (axioms, PDL, target rules) | flat terms within one region; region ports are leaves |
| gate laws (G*, T*, S*) | interfaces — verified per impl at registration/CI |
| extraction (cost / PBQP) | terms + a per-gate `reify` alternative (design-cfg-egraph §3.3) |
| destruction (sea → machine CFG) | Gamma/Theta reification; relocated scf_to_cfg logic |
| pass manager | concrete op types, as today |

## 5. Stage 2 scope (in order)

1. Interface definitions + law obligations wired to the oracle (this doc's §2),
   including the n-ary carrying generalization of scf and plural `LoopLike`.
2. fcc lowers ALL control flow to scf (short-circuit included) — no raw
   `cond_br` reaches the mid-end; `gated_ssa.rs` is deleted.
3. `core/src/sea`: arenas, columns, e-graph, builder from scf/cir; state
   threading per §2.4.
4. isel substrate swap: same TMDL rules and PBQP over sea; Opaque serials die;
   old path behind a flag until every backend LIT and the JIT suite are
   byte-equal or reviewed-better.

Out of scope for Stage 2: EGVN unification (Stage 3), authority flip (Stage 4),
alias classes beyond per-alloca chains, n-way Gamma impls beyond `scf.if`.
