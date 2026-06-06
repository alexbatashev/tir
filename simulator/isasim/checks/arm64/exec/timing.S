# RUN: isasim --march=arm64 --entry=_start --until-pc=done --timing --machine=arm64-ooo %s | filecheck %s
# RUN: isasim --march=arm64 --entry=_start --until-pc=done --timing --machine=arm64-in-order %s | filecheck %s

# AArch64 has its own machine models, so `--timing` works for arm64 too. Four
# instructions retire.
.global done
done:
  add x0, x0, x0
.global _start
_start:
  add x1, x0, 5
  add x2, x0, 7
  add x3, x1, x2
  sub x4, x3, x1

# CHECK: 4 instructions
