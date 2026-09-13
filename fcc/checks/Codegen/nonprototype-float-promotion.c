// RUN: fcc compile --stage ir -o - %s | filecheck %s

int consume();

int call_consume(float value) {
    return consume(value);
}

// CHECK-LABEL: func.func @call_consume
// CHECK: fp.convert {{%[0-9]+}} {semantics = {exceptions = "ignore", kind = "arithmetic", nan = "any_quiet", rounding = "nearest_even", subnormals = "gradual", tininess = "after_rounding"}} : !f64
// CHECK: func.call {{%[0-9]+}}({{%[0-9]+}} : !f64) -> !i32
