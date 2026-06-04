# Bitwise logic and shifts, register and immediate forms.
  addi x1, x0, 12
  addi x2, x0, 10
  and  x3, x1, x2      # 0x8
  or   x4, x1, x2      # 0xe
  xor  x5, x1, x2      # 0x6
  andi x6, x1, 6       # 0x4
  ori  x7, x1, 1       # 0xd
  xori x8, x1, -1      # ~12
  addi x9, x0, 1
  slli x11, x9, 4      # 0x10
  srli x12, x11, 1     # 0x8
  addi x13, x0, -16
  srai x14, x13, 2     # -4 (arithmetic)
  sll  x15, x9, x9     # 1 << 1 = 2
