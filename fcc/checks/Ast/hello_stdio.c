// RUN: fcc compile --stage ast -o - %S/../Inputs/hello_stdio.c | filecheck %s

// CHECK: TranslationUnit
// CHECK: Attribute "_LIBC_SINGLE_BY_DEFAULT()"
// CHECK: Typedef "va_list": Named(__darwin_va_list)
// CHECK: Prototype "printf" ->
// CHECK:     Param _: Attr(restrict; Ptr(Const(Char)))
// CHECK-NEXT:     VarArgs
// CHECK: Typedef "fpos_t":
// CHECK: Struct "__sbuf"
// CHECK-NEXT:     Field "_base":
// CHECK-NEXT:     Field "_size": Int
// CHECK: Struct "__sFILEX"
// CHECK: Struct "__sFILE"
// CHECK:     Field "_close": Ptr(Fn(Ptr(Void)) -> Int)
// CHECK:     Field "_read": Ptr(Fn(Ptr(Void), Attr(_LIBC_COUNT(__n); Ptr(Char)), Int) -> Int)
// CHECK: Typedef "FILE": Record(struct __sFILE)
// CHECK: Global "__stdinp": Ptr(Named(FILE))
// CHECK: Prototype "fclose" -> Int
// CHECK-NEXT:     Param _: Ptr(Named(FILE))
// CHECK: Prototype "fprintf" -> Attr(__printflike(2 , 3); Int)
// CHECK:     VarArgs
// CHECK: Function "main" -> Int
// CHECK-NEXT:     ExprStmt
// CHECK-NEXT:       Call "printf"
// CHECK-NEXT:         String "hello, world\\n"
// CHECK-NEXT:     Return
// CHECK-NEXT:       Int 0
