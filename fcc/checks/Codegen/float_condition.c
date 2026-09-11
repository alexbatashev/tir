// RUN: fcc compile --stage ir -o - %S/../Inputs/codegen_float_condition.c | filecheck %s

// CHECK: constantf {value = 0.0} : !f32
// CHECK: cmpf {{%[0-9]+}}, {{%[0-9]+}} {predicate = "une"} : !i1
