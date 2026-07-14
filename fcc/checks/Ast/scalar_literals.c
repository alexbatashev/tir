// RUN: fcc compile --stage ast -o - %S/../Inputs/scalar_literals.c | filecheck %s

// CHECK: Add
// CHECK-NEXT:         Int 0xffUL
// CHECK-NEXT:         Character 'a'
