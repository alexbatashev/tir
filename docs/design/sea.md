# Sea: TIR's core IR

Status: Stage-2 design, revision 4.

## Architecture

Two levels, red-green style:

**The core (green).** One canonical, data-oriented representation: a region
tree of dialect-defined ops. Ops are what they are today — (dialect, name),
attributes, results, regions, verifiers, interfaces — stored in SoA arenas:
interned op-type ids (u32), payloads and operands/results/regions as CSR
segments, dense u32 ids, per-node version stamps. Persistent by structural
sharing: an edit builds new nodes along the changed spine; untouched subtrees
keep their identity. Single writer per isolated tree; parallelism across
trees. Mutation is the core's own API (create, replace-subtree, wrap, clone,
splice); imperative passes and dialect lowerings program against it directly.

**Views (red), cached in Context, keyed by (node id, version).** A view
renders the core for one purpose. Most are today's cached analyses under a
different invalidation key (dominance, def-use). The e-graph is the one
substantial view: built lazily over a region, it hash-conses pure terms,
saturates with the proved rulesets, and lands its extraction as one atomic
replace-subtree commit. Between commits it may diverge from the core — mid-
saturation it is not IR. Invalidation is proportional to the edited spine, so
edits elsewhere leave it warm. A pass that never asks for it never pays for
it.

**Unstructured control flow is first-class input.** break/continue/goto are
the language. The system restructures totally (Bahmann-Reissmann covers any
CFG, irreducible included); "the core is structured" is a guarantee the
compiler provides, never a restriction on input. A construct that reaches a
fallback path with worse codegen is a bug against the performance targets.
Machine-level CFG exists after destruction in the machine dialects as today.

## Canonical constructs (per the RVSDG specification, Reissmann et al. 2020)

The core's structural vocabulary follows the paper exactly; scf raises onto it:

- **θ (loop)**: tail-controlled, input/output signature equal to the region's
  arguments, region's first result is the continuation predicate; the body
  always runs once. The ONLY loop construct — `scf.for`/`scf.while` raise to
  γ-wrapping-θ; `do` maps directly. (The e-graph view's `SymKind::Theta(init,
  latch)` is the per-value projection of one loop variable through a θ op, not
  the op itself.)
- **γ (decision)**: n-ary from day one — k+1 regions of matching signature,
  integer predicate selects one; symmetric split/join (switch without
  fallthrough is one γ, not a chain).
- **λ/δ/φ/ω (inter-procedural)**: functions are values — a λ's output feeds
  apply-nodes by edges, context variables capture dependencies; δ models
  globals with initializer regions; φ keeps mutual recursion acyclic; ω is the
  translation unit, imports as region arguments, exports as results. Uniform
  def-use at every level: dead-function removal is dead-node elimination, and
  congruence (CNE) extends to structural nodes. No symbol tables in the core.
- **Edges**: value- or state-typed; every input/result is the user of exactly
  one edge; the graph is acyclic, always. Multiple independent states model
  disjoint memory (the per-alloca chains).
- **Raising for arbitrary CFG input** follows Bahmann's control-flow
  restructuring (predicate insertion, no node cloning, no SSA required) +
  structural analysis + demand annotation; fcc emitting structured scf is the
  sanctioned shortcut for limited-control-flow input. Destruction: SCFR
  (today's scf_to_cfg) with PCFR as the future option.

## Extensibility: interfaces, nothing else

Behavior belongs to ops, expressed through the interfaces they implement —
the existing TIR mechanism (`ConstantFold`, `MemoryRead`, `semantic_expr`,
`LoopLike`, `Conditional`). Core consumes interfaces generically and holds no
closed enums and no per-op knowledge. Pass scheduling to concrete op types is
unchanged.

The structured-control interfaces are the ones already landed on this branch:

- `Conditional`: deciding operand + per-region yields aligned with results.
  `scf.if` implements it; any dialect's conditional can.
- `LoopLike` (n-ary): aligned inits / carried args / latched / finals.
  `scf.for`/`scf.while` implement it.
- `MemoryRead`/`MemoryWrite` are read through by the view's memory seeding: an
  access is a term over the chain it reads, which is the whole of memory
  identity there (see "The state laws" below).

## The state laws

`raise` threads one state chain per non-escaping `alloca` plus a conservative
chain; the view seeds every access as a term over the chain it reads —
`LoadMemory(address, bytes, metadata, state)`,
`StoreMemory(address, bytes, value, space, state)` — and unions the operation's
own identity onto the same class. Identity *is* the state operand: two loads
that agree on address and chain hash-cons, two that do not never meet, and no
opaque serial is needed to keep them apart.

- **S1, load-over-store forwarding.** `Load(Store(s, a, n, v), a, n) = v`, at
  one IR type. Congruence is the other half.
- **S2, dead-store elimination.** A write the next write to the same extent
  overwrites leaves the chain as it found it — where nothing else reads that
  chain, which the applier checks against the states the region exports and the
  ones nodes the view did not model consume.
- **S3, disjoint-chain commutation, is structural.** Separate chains never
  appear in each other's terms, so there is nothing to state; it is asserted by
  test.

**These two are definitional, not proved.** Every other equality the view
saturates with is an obligation the `SmtOracle` discharges by bit-blasting to
unsat, and that pipeline is QF_BV: `tir_symbolic::bitblast` reports the memory
kinds unsupported, and the reference interpreter's memory is a concrete byte
array rather than an array model. There is no oracle here to ask, so the laws
are stated as the *meaning* of a read and a write over the state algebra, the
way `Add` means addition — never as an obligation nobody discharged. What keeps
them honest is that both read the state operand, so a law that fires has already
been told the two accesses alias exactly.

Identity is the state edge; the **schedule** is the region's node order. A read
leaves the chain it read, so the state DAG does not order a load against the
write after it, and a commit lands every access where the region held it.

Soundness obligations live where the theory already lives, not in a new
framework: theta axioms are proved by induction in the axiom prover;
speculation through a conditional is guarded at selection legality; a new
interface implementor is exercised by the same LIT/proof suites as the scf
ops.

## Stage 2 remainder

1. mem2reg: structural promotion over `Conditional`/`LoopLike` (in review).
2. fcc: no construct short of goto emits branches (in review); goto
   restructuring closes the gap — in scope, not deferred.
3. `core/src/sea`: the green arenas + mutation API; the e-graph becomes the
   view described above, replacing `SemDagBuilder`'s per-pass rebuild;
   `gated_ssa.rs` deleted (gates read through interfaces).
4. isel substrate swap behind a flag until byte-equal or reviewed-better.
