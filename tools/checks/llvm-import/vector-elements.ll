; RUN: tir llvm-import %s | tir opt --verify | filecheck %s

define float @vector_elements(<2 x float> %value, float %replacement) {
  %squared = fmul <2 x float> %value, %value
  %negated = fneg <2 x float> %squared
  %updated = insertelement <2 x float> %negated, float %replacement, i64 0
  %lane = extractelement <2 x float> %updated, i64 1
  ret float %lane
}

define <2 x float> @build_vector(float %real, float %imaginary) {
  %first = insertelement <2 x float> poison, float %real, i64 0
  %result = insertelement <2 x float> %first, float %imaginary, i64 1
  ret <2 x float> %result
}

; CHECK: func.func @vector_elements
; CHECK: fp.mul
; CHECK: xori
; CHECK: func.func @build_vector
