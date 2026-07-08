// RUN: tir mc --march=arm64 --filetype=obj-ascii %s | filecheck %s
// XFAIL: *

// SILENT DROP (asm path): the `.quad` data directive parses without error but
// emits no bytes (`.byte`, `.word`, `.ascii` work; `.quad` produces nothing).
// A single `.quad 100` should emit 8 little-endian bytes.

.quad 100

// CHECK: [0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
