// RUN: fcc compile --march riscv64 --stage ir -o - %S/../Inputs/integer_conversion.c | filecheck %s

// CHECK: %[[RIGHT:.*]] = ptr.load {{.*}} : !i32
// CHECK: %[[EXTENDED:.*]] = extsi %[[RIGHT]] : !i64
// CHECK: addi {{.*}}, %[[EXTENDED]] : !i64
