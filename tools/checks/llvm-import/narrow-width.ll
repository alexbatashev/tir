; RUN: tir mc --march=riscv64 --stage=isel %s llvm | filecheck %s --check-prefix=RV64
; RUN: tir mc --march=arm64 --stage=isel %s llvm | filecheck %s --check-prefix=ARM64
; RUN: tir mc --march=x86_64 --stage=isel %s llvm | filecheck %s --check-prefix=X86

; Comparisons, right shifts, and divisions read every operand bit. A target
; without the narrow form computes them on extended operands; one with it keeps
; its own instruction.

define i32 @eq8(i8 %a, i8 %b) {
  %c = icmp eq i8 %a, %b
  %z = zext i1 %c to i32
  ret i32 %z
}

define i8 @shr8(i8 %a, i8 %b) {
  %c = lshr i8 %a, %b
  ret i8 %c
}

define i8 @udiv8(i8 %a, i8 %b) {
  %c = udiv i8 %a, %b
  ret i8 %c
}

define i32 @choose(i32 %a, i32 %b, i32 %if_true, i32 %if_false) {
  %c = icmp slt i32 %a, %b
  %r = select i1 %c, i32 %if_true, i32 %if_false
  ret i32 %r
}

; RV64-LABEL: asm.symbol {name = "eq8"
; RV64: riscv.sltiu
; RV64-LABEL: asm.symbol {name = "shr8"
; RV64: riscv.srlw
; RV64-LABEL: asm.symbol {name = "udiv8"
; RV64: riscv.divuw

; ARM64-LABEL: asm.symbol {name = "shr8"
; ARM64: arm64.lsrv_w
; ARM64-LABEL: asm.symbol {name = "udiv8"
; ARM64: arm64.udiv_word
; ARM64-LABEL: asm.symbol {name = "choose"
; ARM64-NEXT: arm64.cmp_w
; ARM64-NEXT: arm64.csel_lt

; X86-LABEL: asm.symbol {name = "shr8"
; X86-NEXT: x86_64.shr_cl8
; X86-LABEL: asm.symbol {name = "choose"
; X86-NEXT: x86_64.cmp32
; X86-NEXT: x86_64.cmovl
