# XFAIL: *
# RUN: not tir mc --march=rv64i --filetype=obj-ascii %s

# MISCOMPILE: an unrecognized mnemonic is silently dropped by the assembler
# instead of raising an error. Here `foobar` (and standard pseudo-instructions
# such as ret/mv/li/la/nop/j/call/not/neg/beqz) produce no encoding and no
# diagnostic; only the surrounding valid instructions are emitted. Silent
# instruction loss can hide typos and unsupported ops. Assembling this file
# should fail; instead it succeeds, so `not` currently fails.

.global f
f:
    addi x1, x2, 1
    foobar x1, x2, x3
    addi x4, x5, 2
