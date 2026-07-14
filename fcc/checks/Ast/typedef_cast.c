// RUN: fcc compile --stage ast -o - %S/../Inputs/typedef_cast.c | filecheck %s

// CHECK: Typedef "word": UnsignedLong
// CHECK: Function "convert" -> Named(word)
// CHECK: Cast Named(word)
