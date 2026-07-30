// RUN: fcc compile --stage ast -o - %S/../Inputs/array_parameter_definition.c | filecheck %s

// CHECK: Function "main" -> Int
// CHECK-NEXT:     Param "argc": Int
// CHECK-NEXT:     Param "argv": Array(Ptr(Char))
