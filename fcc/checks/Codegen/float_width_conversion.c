// RUN: fcc compile --stage ir -o - %S/../Inputs/codegen_float_width_conversion.c | filecheck %s

// CHECK: func.func @narrow({{%[0-9]+}}: !f64) -> !f32
// CHECK: fcvt {{%[0-9]+}} : !f32
// CHECK: func.func @widen({{%[0-9]+}}: !f32) -> !f64
// CHECK: fcvt {{%[0-9]+}} : !f64
