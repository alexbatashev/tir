// RUN: fcc compile --stage ir -o - %s | filecheck %s

int consume();

int call_consume(float value) {
    return consume(value);
}

// CHECK-LABEL: func.func @call_consume
// CHECK: fcvt {{%[0-9]+}} : !f64
// CHECK: func.call {{%[0-9]+}}({{%[0-9]+}} : !f64) -> !i32
