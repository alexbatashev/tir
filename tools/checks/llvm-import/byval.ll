; RUN: tir llvm-import %s | tir opt --verify | filecheck %s
; RUN: tir mc --march x86_64 --filetype obj %s llvm -o /tmp/tir-byval-lit.o
; RUN: cc /tmp/tir-byval-lit.o %S/Inputs/byval-harness.c -o /tmp/tir-byval-lit
; RUN: /tmp/tir-byval-lit

%S = type { i64, i64, i64, i64 }

define i64 @sum_byval(ptr byval(%S) align 8 %s) {
  %a_ptr = getelementptr %S, ptr %s, i32 0, i32 0
  %d_ptr = getelementptr %S, ptr %s, i32 0, i32 3
  %a = load i64, ptr %a_ptr, align 8
  %d = load i64, ptr %d_ptr, align 8
  %result = add i64 %a, %d
  ret i64 %result
}

; CHECK: func.func @sum_byval
; CHECK: argument_alignments

define i64 @call_sum_byval(ptr %s) {
  %result = call i64 @sum_byval(ptr byval(%S) align 8 %s)
  ret i64 %result
}

; CHECK: func.func @call_sum_byval
; CHECK: argument_alignments
