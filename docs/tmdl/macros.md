# TMDL Declarative Macros

This document describes TMDL's `macro_rules!`-style macro system: a token-level
rewriting pass that runs between lexing and parsing. It is intended as a concise,
example-driven reference. For the surrounding grammar, see the
[Syntax Guide](syntax.md).

## Motivation

Templates (single-inheritance mixins) capture RISC-V's fixed 32-bit encoding
well, but cannot express CISC width variance: on x86 the encoding *shape* changes
with operand width — the REX nibble appears or disappears, the `0x66` prefix
shifts every bit offset, and register classes switch. The result is heavy
boilerplate: the same ALU mnemonic repeated across five widths, templates
quintupled per width, and a REX scaffold copy-pasted into every hand-rolled
encoding. Macros are an orthogonal tool that rewrites token streams before the
parser runs, letting one definition stand in for a family of near-identical
instructions or templates while templates stay untouched.

## Definition and Invocation

A macro is a top-level item introduced by `macro`. Its body is a list of arms,
each `(matcher) => { transcription };`. Arms are tried in order; the first whose
matcher accepts the input wins.

```
macro NAME {
    (matcher_1) => { transcription_1 };
    (matcher_2) => { transcription_2 };
}
```

Invoke with `NAME!(...)`, `NAME!{...}`, or `NAME![...]` — the three delimiters
are interchangeable. Invocations are recognized at **any nesting depth**, so a
macro may expand in item position, in statement position inside a `behavior`
block, or in encoding position. The expander output is re-scanned, so a macro
may itself emit further invocations.

An invocation is only recognized when an identifier is immediately followed by
`!` and an opening delimiter **and** the name is a known macro. `a != b` is never
mistaken for an invocation.

## Fragment Specifiers

Matchers bind input to metavariables written `$name:kind`. Three kinds exist:

- `$x:ident` — a single identifier token.
- `$x:literal` — a single literal token (integer or string).
- `$x:tt` — one token tree: either a single token or a whole delimited group,
  captured verbatim. `$($x:tt)*` is the escape hatch for arbitrary fragments
  (there is no `expr` fragment).

A matcher that expects `ident` rejects a `literal` argument, so overloaded arms
can dispatch on fragment kind.

## Repetition

`$( ... )*` and `$( ... )+` match a repeated group, with an optional single-token
separator between the `)` and the `*`/`+`. Repetitions may nest. A transcription
repetition must iterate over a metavariable bound in the matching repetition, and
all metavariables driven by one repetition must have equal length.

Real example from the x86 defs — one row per instruction, expanded into one
`instruction` item each:

```
macro alu_rr {
    ( $tmpl:ident, $( ( $name:ident, $mn:literal, $opc:literal, { $($b:tt)* } ) ),* ) => {
        $( instruction $name for [X86_64] : $tmpl {
               param MNEMONIC: String = $mn;
               param OPCODE: bits<8> = $opc;
               behavior { $($b)* }
           } )*
    };
}

alu_rr!(RegReg,
    (Add, "add", 0x01, { dst = dst + src; }),
    (Sub, "sub", 0x29, { dst = dst - src; }),
    (Mov, "mov", 0x89, { dst = src; })
)
```

The `,` before `*` is the repetition separator; `{ $($b:tt)* }` captures each
behavior body verbatim.

## Identifier and String Concatenation

`${concat(a, b, ...)}` joins its arguments into one token. The **result token
kind is the kind of the first argument**:

- `${concat($name, 32)}` where `$name` is `Add` yields the identifier `Add32`.
- `${concat($mn, $osuf)}` where `$mn` is `"add"` yields the string `"add32"`.

Synthesized strings are interned in a `StringArena` for the duration of the
compile; no leaking is involved. This is what lets width-suffixed instruction
names (`Add` → `Add32`) fall out of a single template.

## Cross-File Visibility

Macros are visible across all input files, like other TMDL items. Collection is
two-phase: every input is lexed and all `macro` definitions gathered into one
table (a duplicate name is an error), then each file is expanded against that
table. Order between files does not matter — a file may invoke a macro defined
in a later-listed file.

## No Hygiene

The macro system is deliberately **not hygienic**. Capture is the point: a
`{ dst = dst + src; }` fragment must bind to the `dst`/`src` operands introduced
by the template it is spliced into. TMDL has no binder forms that could be
captured accidentally, and duplicate item names are already diagnosed by
semantic analysis.

## Spans and Diagnostics

Expanded tokens carry the **invocation-site span**, not the definition-site span.
For a macro defined in one file and invoked in another, a definition-site span
would point diagnostics at the wrong file; the invocation site is where a user
looks to fix an error. Expander diagnostics share the shape used by semantic
analysis and type checking and render through the same machinery. Errors
reported include unbalanced delimiters, duplicate macro, no matching arm, unknown
metavariable, repetition length mismatch, unknown macro invoked, a bad
`${concat}` argument, and the resource limits below.

## Limits

Expansion is bounded to keep pathological inputs from hanging the compiler:

- **Recursion depth 64** — how many times output may be re-scanned and
  re-expanded.
- **1,000,000 tokens** per file after expansion.
- **Delimiter nesting 128** — maximum depth of `()`/`[]`/`{}` groups.
- **Match-step budget** — matcher backtracking is capped so a degenerate arm
  aborts quickly instead of running for a long time.

Note that `<` and `>` are **not** delimiters (so `bits<8>` tokenizes normally);
only `()`, `[]`, and `{}` form token-tree groups.

## Trailing Semicolon

An invocation in **item position takes no trailing `;`** — it stands where the
items it expands to would stand:

```
alu_rr!(RegReg,
    (Add, "add", 0x01, { dst = dst + src; })
)
```

Likewise a statement- or encoding-position invocation takes no `;` of its own;
the transcription supplies whatever punctuation the spliced tokens need. For
instance `sub_flags!({ src }, { self.XLEN - 1 })` inside a `behavior` block
expands to statements that already carry their own semicolons.
