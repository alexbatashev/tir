# RUN: not tir mc --march=x86_64 --filetype=obj-ascii %s

# Unknown assembly mnemonics are hard parse errors.

	.global x
x:
	foobar rax, rbx
	add rax, rbx
