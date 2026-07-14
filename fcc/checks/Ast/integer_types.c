// RUN: fcc compile --stage ast -o - %S/../Inputs/integer_types.c | filecheck %s

// CHECK: Function "widen" -> UnsignedLongLong
// CHECK-NEXT:     Param "value": UnsignedShort
// CHECK-NEXT:     Decl "result": UnsignedLongLong
