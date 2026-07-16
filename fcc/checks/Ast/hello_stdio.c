// RUN: fcc compile --stage ast -I %S/../Inputs -o - %S/../Inputs/hello_stdio.c | filecheck %s

// CHECK: Prototype "printf" ->
// CHECK:     Param _: Attr(restrict; Ptr(Const(Char)))
// CHECK-NEXT:     VarArgs
// CHECK: Function "main" -> Int
// CHECK-NEXT:     ExprStmt
// CHECK-NEXT:       Call "printf"
// CHECK-NEXT:         String "hello, world\\n"
// CHECK-NEXT:     Return
// CHECK-NEXT:       Int 0
