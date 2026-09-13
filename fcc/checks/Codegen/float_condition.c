// RUN: fcc compile --stage ir -o - %S/../Inputs/codegen_float_condition.c | filecheck %s

// CHECK: fp.constant {bits = 0} : !f32
// CHECK: fp.cmp {{%[0-9]+}}, {{%[0-9]+}} {predicate = "une", semantics = {behavior = "quiet", exceptions = "ignore", kind = "comparison", subnormals = "gradual"}} : !i1
