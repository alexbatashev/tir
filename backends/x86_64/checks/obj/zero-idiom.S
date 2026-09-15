# RUN: tir mc --march=x86_64 --filetype=obj-ascii %s | filecheck %s

	.global zero_high
zero_high:
	xor r14d, r14d

# CHECK: zero_high:
# CHECK-NEXT: [0x45, 0x31, 0xF6]
