# Loads and stores through the scratch pointer in x10.
# Exercises memory-window comparison: bytes written here must match the golden
# model's memory.
  addi x1, x0, 0x42
  sd   x1, 0(x10)         # store doubleword at scratch+0
  addi x2, x0, 0x7f
  sw   x2, 8(x10)         # store word at scratch+8
  addi x3, x0, -1
  sb   x3, 16(x10)        # store byte 0xff at scratch+16
  ld   x4, 0(x10)         # reload doubleword -> 0x42
  lw   x5, 8(x10)         # reload word -> 0x7f
  lbu  x6, 16(x10)        # reload byte unsigned -> 0xff
  lb   x7, 16(x10)        # reload byte signed -> -1
