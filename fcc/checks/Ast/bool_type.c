// RUN: fcc compile -std=c99 --stage ast -o - %S/../Inputs/bool_type.c | filecheck %s

// CHECK: Function "negate" -> Bool
// CHECK-NEXT:     Param "value": Bool
