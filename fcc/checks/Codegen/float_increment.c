// RUN: fcc compile --stage ir -o - %s | filecheck %s

float increment(float value) {
    return ++value;
}

// CHECK-LABEL: func.func @increment
// CHECK: constantf {value = 1.0} : !f32
// CHECK: addf {{%[0-9]+}}, {{%[0-9]+}} : !f32
