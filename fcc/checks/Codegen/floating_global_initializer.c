// RUN: fcc compile --stage ir -o - %s | filecheck %s

float narrow = -0.0f;
double wide = 1.5;
static float hidden = 2.5f;
static float zeroed;

// CHECK: global @narrow align 4 bytes [0, 0, 0, 128]
// CHECK: global @wide align 8 bytes [0, 0, 0, 0, 0, 0, 248, 63]
// CHECK: global private @hidden align 4 bytes [0, 0, 32, 64]
// CHECK: global private @zeroed size 4 align 4
