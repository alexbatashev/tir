// RUN: fcc compile --stage ir -o - %S/../Inputs/codegen_goto.c | filecheck %s

// A `goto` records the label it names in the pending-jump slot; the label a
// `goto` reaches from a later statement puts the rest of the sequence in a
// dispatch loop that resumes there. Nothing branches.

// CHECK: func @sum_to
// CHECK: cir.do
// CHECK: cir.condition
// CHECK: return
// CHECK-NOT: cir.goto
// CHECK-NOT: cir.label
