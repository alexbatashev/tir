// RUN: fcc compile --stage ast -std=gnu17 -o - %S/../Inputs/deep_record_nesting.c | filecheck %s

// CHECK: TranslationUnit
// CHECK: Struct "s0000"
// CHECK: Field "x": Record(struct s0001)
