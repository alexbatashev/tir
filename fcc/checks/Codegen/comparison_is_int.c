// RUN: fcc compile -O0 --stage ir -o - %s | filecheck %s

// A relational operator has type `int` in C, while the machine predicate it
// lowers to is one bit wide. Every consumer reads the operand's C type, so the
// widening belongs here rather than in each of them: an operand left one bit
// wide reaches a binary operator whose other side is an `int`, a call whose
// parameter is declared `int`, and a slot four bytes wide.

int compare(int a)
{
    return a < 2;
}

// CHECK-LABEL: func.func @compare
// CHECK: %[[P:[0-9]+]] = cmpi {{.*}} : !i1
// CHECK: extui %[[P]]{{.*}} : !i32

int compound(int c)
{
    int d = 3;
    d ^= c < 2;
    return d;
}

// CHECK-LABEL: func.func @compound
// CHECK: cmpi {{.*}} : !i1
// CHECK: extui {{.*}} : !i32
// CHECK: xori
