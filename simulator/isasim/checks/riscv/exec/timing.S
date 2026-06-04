# RUN: isasim --march=riscv64 --entry=_start --until-pc=done --timing --machine=ooo %s | filecheck %s
# RUN: isasim --march=riscv64 --entry=_start --until-pc=done --timing --machine=in-order %s | filecheck %s

# The cycle-approximate timing model runs after the functional pass and reports
# the dynamic instruction count and cycle estimate. Three instructions retire.
.global done
done:
  add x0, x0, x0
.global _start
_start:
  addi x1, x0, 5
  addi x2, x0, 7
  add  x3, x1, x2

# CHECK: 3 instructions
