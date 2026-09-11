// RUN: fcc compile --stage ir -o - %S/../Inputs/codegen_global_float_assignment.c | filecheck %s

// CHECK: global @total
// CHECK-LABEL: func.func @set_total
// CHECK: ptr.store {{%[0-9]+}}, {{%[0-9]+}}
