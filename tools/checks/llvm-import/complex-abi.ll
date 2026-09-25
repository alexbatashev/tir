; RUN: tir llvm-import %s | tir opt --verify | filecheck %s

define <2 x float> @complex_identity(<2 x float> %value) {
  ret <2 x float> %value
}

; CHECK: func.func @complex_identity(%{{[0-9]+}}: !vector.vec<2xf32>) -> !vector.vec<2xf32>

define float @negate(float %value) {
  %negated = fneg float %value
  ret float %negated
}

; CHECK: func.func @negate
; CHECK: bitcast
; CHECK: xori
; CHECK: bitcast

define i32 @first(i32 %value, ...) {
  ret i32 %value
}

; CHECK: func.func @first

declare float @llvm.fabs.f32(float)
declare float @llvm.fmuladd.f32(float, float, float)

define float @complex_math(float %x, float %y, float %z) {
  %absolute = call float @llvm.fabs.f32(float %x)
  %product_sum = call float @llvm.fmuladd.f32(float %absolute, float %y, float %z)
  ret float %product_sum
}

; CHECK: func.func @complex_math
; CHECK: fp.abs
; CHECK: fp.mul
; CHECK: fp.add

define void @dead_end() {
  unreachable
}

; CHECK: func.func @dead_end
; CHECK: cfg.br ^bb0

define i1 @has_nan(float %a, float %b) {
  %nan = fcmp uno float %a, %b
  ret i1 %nan
}

; CHECK: func.func @has_nan
; CHECK: fp.cmp
; CHECK: fp.cmp
; CHECK: ori
