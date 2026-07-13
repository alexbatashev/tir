# Writing good backends

Backends use TMDL to describe target ISA. See documentation or compiler code for
syntax. Use existing code as an example of what is possible in TMDL today.

General rules:

- Avoid duplication. Prefer templates over macros.
- One behavior = one instruction. Mnemonics can be duplicate. If an instruction
  requires dynamic encoding width - that's probably two separate instructions
  doing different things. Split them up, reuse mnemonics but define separate
  encodings and behaviors.
- Avoid adding escape hatches to ISel, use existing generic mechanism.
