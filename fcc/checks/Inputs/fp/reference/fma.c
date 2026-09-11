#pragma STDC FENV_ACCESS ON

#include <fenv.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static double separate(double a, double b, double c) {
  volatile double product = a * b;
  return product + c;
}

static uint64_t bits(double value) {
  uint64_t result;
  memcpy(&result, &value, sizeof(result));
  return result;
}

int main(int argc, char **argv) {
  if (argc != 2)
    return 2;
  double a = 0x1.000002p+0;
  double b = 0x1.ffffffcp-1;
  double c = -0x1p+0;
  feclearexcept(FE_ALL_EXCEPT);
  double result = strcmp(argv[1], "fused") == 0 ? fma(a, b, c) : separate(a, b, c);
  int flags = fetestexcept(FE_ALL_EXCEPT);
  printf("{\"kind\":\"exact_bits\",\"bits\":\"0x%016llx\",\"flags\":[",
         (unsigned long long)bits(result));
  if (flags & FE_INEXACT)
    printf("\"inexact\"");
  printf("]}\n");
  return 0;
}
