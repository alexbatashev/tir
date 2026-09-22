; RUN: tir mc --march=x86_64 --filetype=asm %s | filecheck %s
; RUN: tir mc --march=x86_64 --shuffle-machine-order --filetype=obj %s -o /dev/null

define i32 @select_low_bit(i32 %a) {
  %c = icmp eq i32 %a, 2
  %r = select i1 %c, i32 2, i32 3
  ret i32 %r
}
define i32 @select_low_bit_reverse(i32 %a) {
  %c = icmp eq i32 %a, 2
  %r = select i1 %c, i32 3, i32 2
  ret i32 %r
}

; CHECK-LABEL: select_low_bit:
; CHECK: sete
; CHECK-NOT: shl
; CHECK-NOT: sar
; CHECK: xor
; CHECK: ret
; CHECK-LABEL: select_low_bit_reverse:
; CHECK: sete
; CHECK-NOT: shl
; CHECK-NOT: sar
; CHECK: xor
; CHECK: ret

; This is not a complemented mask and must not use the blend identity.
define i32 @other_mask(i32 %mask) {
  %left = and i32 %mask, 85
  %different = xor i32 %mask, 3
  %right = and i32 %different, 170
  %result = or i32 %left, %right
  ret i32 %result
}
