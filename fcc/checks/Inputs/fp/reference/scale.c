#pragma STDC FENV_ACCESS ON

#include <fenv.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static uint64_t bits(double value) {
  uint64_t result;
  memcpy(&result, &value, sizeof(result));
  return result;
}

static double from_bits(const char *text) {
  uint64_t value = strtoull(text, NULL, 0);
  double result;
  memcpy(&result, &value, sizeof(result));
  return result;
}

static void flag(const char *name, int *first) {
  printf("%s\"%s\"", *first ? "" : ",", name);
  *first = 0;
}

int main(int argc, char **argv) {
  if (argc != 2)
    return 2;
  volatile double input = from_bits(argv[1]);
  feclearexcept(FE_ALL_EXCEPT);
  volatile double result = input / 2.0;
  int flags = fetestexcept(FE_ALL_EXCEPT);
  printf("{\"kind\":\"exact_bits\",\"bits\":\"0x%016llx\",\"flags\":[",
         (unsigned long long)bits(result));
  int first = 1;
  if (flags & FE_INVALID)
    flag("invalid", &first);
  if (flags & FE_DIVBYZERO)
    flag("divide_by_zero", &first);
  if (flags & FE_OVERFLOW)
    flag("overflow", &first);
  if (flags & FE_UNDERFLOW)
    flag("underflow", &first);
  if (flags & FE_INEXACT)
    flag("inexact", &first);
  printf("]}\n");
  return 0;
}
