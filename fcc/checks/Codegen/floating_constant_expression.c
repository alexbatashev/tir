// RUN: fcc compile --stage ir -o - %s | filecheck %s

float rounded = 16777216.0f + 1.0f;
double converted = (float)0x1.000003p0;
float converted_after_expression = (16777216.0 + 3.0) - 1.0;

// CHECK: global @rounded align 4 bytes [0, 0, 128, 75]
// CHECK: global @converted align 8 bytes [0, 0, 0, 64, 0, 0, 240, 63]
// CHECK: global @converted_after_expression align 4 bytes [1, 0, 128, 75]
