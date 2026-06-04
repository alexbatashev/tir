# Integer arithmetic: add/sub and immediate forms.
# Harness contract: x1..x31 start at 0, x10 holds the scratch base.
  addi x1, x0, 5
  addi x2, x0, 7
  add  x3, x1, x2      # 12
  sub  x4, x2, x1      # 2
  addi x5, x1, -8      # -3 (sign extended)
  add  x6, x5, x5      # -6
  sub  x7, x0, x1      # -5
