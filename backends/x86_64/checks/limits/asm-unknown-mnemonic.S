# RUN: not tir mc --march=x86_64 --filetype=obj-ascii %s
# XFAIL: *

# FINDING R1 (robustness): an unknown assembly mnemonic is silently dropped with
# no diagnostic and exit 0, so the following `add` is assembled as if the bad
# line were absent. A garbage instruction should be a hard error; today it is
# not, so `not tir` fails and this test is XFAIL. When fixed it will XPASS.

	.global x
x:
	foobar rax, rbx
	add rax, rbx
