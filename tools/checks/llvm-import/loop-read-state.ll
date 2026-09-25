; RUN: tir mc --march x86_64 --filetype obj %s llvm -o /tmp/tir-loop-read-state-lit.o
source_filename = "fcc/extbench/libquantum/src/qureg.c"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-pc-linux-gnu"

%struct.quantum_reg_struct = type { i32, i32, i32, ptr, ptr }
%struct.quantum_reg_node_struct = type { { float, float }, i64 }

@.str.9 = external hidden unnamed_addr constant [10 x i8], align 1

declare i32 @printf(ptr noundef, ...) #0

; Function Attrs: noinline nounwind sspstrong uwtable
define dso_local void @quantum_print_expn(ptr noundef byval(%struct.quantum_reg_struct) align 8 %0) #1 {
  %2 = alloca i32, align 4
  store i32 0, ptr %2, align 4
  br label %3

3:                                                ; preds = %26, %1
  %4 = load i32, ptr %2, align 4
  %5 = getelementptr inbounds nuw %struct.quantum_reg_struct, ptr %0, i32 0, i32 1
  %6 = load i32, ptr %5, align 4
  %7 = icmp slt i32 %4, %6
  br i1 %7, label %8, label %29

8:                                                ; preds = %3
  %9 = load i32, ptr %2, align 4
  %10 = getelementptr inbounds nuw %struct.quantum_reg_struct, ptr %0, i32 0, i32 3
  %11 = load ptr, ptr %10, align 8
  %12 = load i32, ptr %2, align 4
  %13 = sext i32 %12 to i64
  %14 = getelementptr inbounds %struct.quantum_reg_node_struct, ptr %11, i64 %13
  %15 = getelementptr inbounds nuw %struct.quantum_reg_node_struct, ptr %14, i32 0, i32 1
  %16 = load i64, ptr %15, align 8
  %17 = load i32, ptr %2, align 4
  %18 = getelementptr inbounds nuw %struct.quantum_reg_struct, ptr %0, i32 0, i32 0
  %19 = load i32, ptr %18, align 8
  %20 = sdiv i32 %19, 2
  %21 = shl i32 1, %20
  %22 = mul nsw i32 %17, %21
  %23 = sext i32 %22 to i64
  %24 = sub i64 %16, %23
  %25 = call i32 (ptr, ...) @printf(ptr noundef @.str.9, i32 noundef %9, i64 noundef %24)
  br label %26

26:                                               ; preds = %8
  %27 = load i32, ptr %2, align 4
  %28 = add nsw i32 %27, 1
  store i32 %28, ptr %2, align 4
  br label %3, !llvm.loop !6

29:                                               ; preds = %3
  ret void
}

attributes #0 = { "frame-pointer"="all" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "target-cpu"="x86-64" "target-features"="+cmov,+cx8,+fxsr,+mmx,+sse,+sse2,+x87" "tune-cpu"="generic" }
attributes #1 = { noinline nounwind sspstrong uwtable "frame-pointer"="all" "min-legal-vector-width"="0" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "target-cpu"="x86-64" "target-features"="+cmov,+cx8,+fxsr,+mmx,+sse,+sse2,+x87" "tune-cpu"="generic" }

!llvm.module.flags = !{!0, !1, !2, !3, !4}
!llvm.ident = !{!5}

!0 = !{i32 1, !"wchar_size", i32 4}
!1 = !{i32 8, !"PIC Level", i32 2}
!2 = !{i32 7, !"PIE Level", i32 2}
!3 = !{i32 7, !"uwtable", i32 2}
!4 = !{i32 7, !"frame-pointer", i32 2}
!5 = !{!"clang version 22.1.8"}
!6 = distinct !{!6, !7}
!7 = !{!"llvm.loop.mustprogress"}
