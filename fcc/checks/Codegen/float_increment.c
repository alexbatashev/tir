// RUN: fcc compile --stage ir -o - %s | filecheck %s

float increment(float value) {
    return ++value;
}

// CHECK-LABEL: func.func @increment
// CHECK: fp.constant {bits = 1065353216} : !f32
// CHECK: fp.add {{%[0-9]+}}, {{%[0-9]+}} : !f32
