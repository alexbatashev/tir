// RUN: not tir mc --march=arm64 --filetype=obj-ascii %s

// Unsupported aliases must fail instead of disappearing from the output.

f:
    mov x0, x1
    ret x30
