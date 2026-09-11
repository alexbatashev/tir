// RUN: fcc compile --stage ir -o - %s | filecheck %s

float tail(void) {
    float values[3] = {1.0f};
    return values[2];
}

// CHECK-LABEL: func.func @tail
// CHECK: constantf {value = 0.0} : !f32
