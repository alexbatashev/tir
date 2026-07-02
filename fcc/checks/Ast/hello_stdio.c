// RUN: fcc compile --stage ast -o - %S/../Inputs/hello_stdio.c | filecheck %s

// CHECK: TranslationUnit
// CHECK-NEXT:   Prototype "printf" -> Int
// CHECK-NEXT:     Param "format": Ptr(Const(Char))
// CHECK-NEXT:     VarArgs
// CHECK-NEXT:   Function "main" -> Int
// CHECK-NEXT:     ExprStmt
// CHECK-NEXT:       Call "printf"
// CHECK-NEXT:         String "hello, world\\n"
// CHECK-NEXT:     Return
// CHECK-NEXT:       Int 0
