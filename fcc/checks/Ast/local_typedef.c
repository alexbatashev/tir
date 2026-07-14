// RUN: fcc compile --stage ast -o - %S/../Inputs/local_typedef.c | filecheck %s

// CHECK: Typedef "word": UnsignedLong
// CHECK-NEXT:     Return
// CHECK-NEXT:       Cast Named(word)
