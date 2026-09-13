// RUN: fcc compile --stage ir -o - %S/../Inputs/codegen_float_width_conversion.c | filecheck %s

// CHECK: func.func @narrow({{%[0-9]+}}: !f64) -> !f32
// CHECK: fp.convert {{%[0-9]+}} {semantics = {exceptions = "ignore", kind = "arithmetic", nan = "any_quiet", rounding = "nearest_even", subnormals = "gradual", tininess = "after_rounding"}} : !f32
// CHECK: func.func @widen({{%[0-9]+}}: !f32) -> !f64
// CHECK: fp.convert {{%[0-9]+}} {semantics = {exceptions = "ignore", kind = "arithmetic", nan = "any_quiet", rounding = "nearest_even", subnormals = "gradual", tininess = "after_rounding"}} : !f64
