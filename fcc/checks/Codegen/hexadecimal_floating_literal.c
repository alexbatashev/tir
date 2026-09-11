// RUN: fcc compile --stage ir -o - %s | filecheck %s

float rounded = 0x1.000001p0f;
double exact = 0x1.8p+1;

// CHECK: global @rounded align 4 bytes [0, 0, 128, 63]
// CHECK: global @exact align 8 bytes [0, 0, 0, 0, 0, 0, 8, 64]
