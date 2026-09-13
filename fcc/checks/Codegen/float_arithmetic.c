// RUN: fcc compile --stage ir -o - %S/../Inputs/codegen_float_arithmetic.c | filecheck %s

// CHECK: func.func @add({{%[0-9]+}}: !f32, {{%[0-9]+}}: !f32) -> !f32
// CHECK: ptr.alloca {size = 4, align = 4}
// CHECK: ptr.store {{%[0-9]+}}, {{%[0-9]+}}
// CHECK: ptr.load {{%[0-9]+}} {{.*}} : !f32
// CHECK: fp.add {{%[0-9]+}}, {{%[0-9]+}} {semantics = {exceptions = "ignore", kind = "arithmetic", nan = "any_quiet", rounding = "nearest_even", subnormals = "gradual", tininess = "after_rounding"}} : !f32
