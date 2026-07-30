// RUN: fcc compile --stage ast -std=gnu17 -o - %S/../Inputs/opaque_array_bounds.c | filecheck %s

// CHECK: TranslationUnit
// CHECK: Global "aligned" extern=false: Array(Int, __alignof__ ( item ) >= __alignof__ ( int ) ? 1 : - 1)
// CHECK: Global "compound" extern=false: Array(Int, sizeof ( struct Item ) { 1 } == sizeof ( struct Item ) ? 1 : - 1)
// CHECK: Field "data": Array(Char, sizeof ( struct Item ) - __builtin_offsetof ( struct Item , value ))
