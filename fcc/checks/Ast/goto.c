// RUN: fcc compile --stage ast -o - %S/../Inputs/goto.c | filecheck %s

// CHECK: Goto "done"
// CHECK: Label "done"
// CHECK-NEXT:       Return
