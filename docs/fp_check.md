# Floating-point reference checks

`cargo xtask fp-check` records compiler observations and compares them with
independent semantic expectations. The case manifest lives at
`fcc/checks/Inputs/fp/cases.toml`. Stable case IDs and stage ownership let later
compiler stages extend the same data set. The manifest also keeps the release
inventory for wider binary formats, decimal and complex arithmetic, remaining
math functions, target options, long-double ABIs, and feature macros.

## Record GCC observations

Run the pinned GCC 15.2 profile:

```sh
cargo xtask fp-check reference --gcc gcc --output /tmp/fp-reference.json
```

The command rejects another compiler version. To record another compiler, pass
`--profile` with a name other than `gcc-15.2`. This creates separate evidence
and does not replace the pinned profile.

Use `--case ID` to run one case. The runner passes every compiler and runtime
argument directly to a process. It does not use a shell. Each probe runs in an
isolated temporary directory. A failed compiler or probe keeps that directory
and writes its path to `artifacts`. Setup failures still write a report, marking
every selected case as `missing_infrastructure`, before returning nonzero.

The reference report records the manifest and source SHA-256 digests, compiler
version and executable path, target, C library, commands, exit status,
observation, and status. The manifest digest binds evidence to case arguments,
inputs, expectations, and stage ownership. GCC-specific expectations use
`reference_expectation`. The independent `expectation` remains unchanged when
GCC behavior differs.

## Check a stage

Run the cumulative reference-stage requirements:

```sh
cargo xtask fp-check check \
  --stage reference \
  --reference /tmp/fp-reference.json \
  --output /tmp/fp-stage1.json
```

Stage names are `reference`, `scalar`, `round`, `fcc`, `math`, `rules`,
`vector`, and `release`. A missing required result, failed comparison,
unsupported required capability, missing tool, or empty selection makes the
command fail. The command writes the report before returning a comparison
failure. Step 1 implements only the `reference` checker. It reports selected
post-reference TIR cases as unsupported until their owning stages add a TIR
observation path. Pass `--case ID` to reproduce one selected case.
Before comparing observations, the checker verifies the recorded compiler,
source digest, stage, host identity, and command provenance against the selected
manifest and current source.

Manifest expectations use these kinds:

- `exact_bits` records result bits and exception flags.
- `permitted_set` records every allowed result.
- `correlated_results` records outputs that must come from one evaluation and
  therefore must match together.
- `numerical_bound` records the metric, domain, limit, zero and subnormal
  conventions, and exceptional-value behavior.
- `code_shape` records required and forbidden instruction text.
- `effects` records scalar or complete vector result bits when relevant, flags,
  errno, ordered events, and trap delivery.
- `diagnostic` records required diagnostic text.

Every report result has one status. `pass` and `fail` are comparison outcomes.
`unsupported_capability` means the required operation or target mode does not
exist. `missing_infrastructure` means a required compiler, library, oracle, or
saved observation is absent. The last two statuses never count as passes.

## Read a saved report

Summarize a report without running a compiler:

```sh
cargo xtask fp-check report /tmp/fp-stage1.json
```

The command prints status counts and every non-passing case. It fails when any
case is not `pass` or when the report has no cases.

## Capture the Whetstone baseline

The Whetstone manifest uses `1000000` as its runtime argument. Capture both
configured compilers without editing the benchmark:

```sh
cargo xtask extbench run -p fcc -b whetstone --compiler gcc \
  --output /tmp/whetstone-gcc-baseline.json
cargo xtask extbench run -p fcc -b whetstone --compiler fcc \
  --output /tmp/whetstone-fcc-baseline.json
```

The JSON stores `wall_ms` from `Instant` in fractional milliseconds and
`peak_rss_kb` from `/usr/bin/time`. The terminal summary prints milliseconds to
three decimal places. Compare results only on the same host with the same
compiler versions, benchmark revision, arguments, and library.
