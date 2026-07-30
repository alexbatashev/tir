// RUN: fcc compile --stage ast -o - %S/../Inputs/adjacent_strings.c | filecheck %s

// CHECK: String "same line"
// CHECK: String "cross line"
// CHECK: String "macro expansion"
