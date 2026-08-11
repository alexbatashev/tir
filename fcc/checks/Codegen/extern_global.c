// RUN: fcc compile --stage ir -o - %s | filecheck %s --check-prefix=IR
// RUN: fcc compile --march x86_64 --stage asm -o - %s | filecheck %s

// An object defined in another translation unit is declared here, so the
// address taken of it names a symbol the module knows.

extern int counter;

int bump(void)
{
  return counter + 1;
}

// IR: cir.extern_global {sym_name = "counter"}
// IR: addr_of {sym_name = "counter"}

// CHECK-LABEL: bump:
// CHECK: counter
