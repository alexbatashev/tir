// RUN: fcc compile --stage ir -I %S/../Inputs -o - %S/../Inputs/hello_stdio.c | filecheck %s

// CHECK: declare @printf(!ptr.p<!i8>) -> !i32
// CHECK: func @main() -> !i32 {
// CHECK: cir.string {value = "hello, world\n"} : !ptr.p<!i8>
// CHECK: call @printf(%{{[0-9]+}} : !ptr.p<!i8>) -> !i32
// CHECK: return
