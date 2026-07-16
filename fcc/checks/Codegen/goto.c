// RUN: fcc compile --stage ir -o - %S/../Inputs/codegen_goto.c | filecheck %s

// CHECK: func @sum_to
// CHECK: cir.label {label = "again"}
// CHECK: cir.if
// CHECK: cir.goto {label = "done"}
// CHECK: cir.goto {label = "again"}
// CHECK: cir.label {label = "done"}
// CHECK: return
