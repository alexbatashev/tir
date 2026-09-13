// RUN: fcc compile --stage ir -o - %s | filecheck %s

float increment(float value) {
    return ++value;
}

// CHECK-LABEL: func.func @increment
// CHECK: fp.constant {bits = 1065353216} : !f32
// CHECK: fp.add {{%[0-9]+}}, {{%[0-9]+}} {semantics = {exceptions = "ignore", kind = "arithmetic", nan = "any_quiet", rounding = "nearest_even", subnormals = "gradual", tininess = "after_rounding"}} : !f32
