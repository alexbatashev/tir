#pragma STDC FENV_ACCESS ON

#include <errno.h>
#include <fenv.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static double from_bits(const char *text) {
  uint64_t value = strtoull(text, NULL, 0);
  double result;
  memcpy(&result, &value, sizeof(result));
  return result;
}

static uint64_t bits(double value) {
  uint64_t result;
  memcpy(&result, &value, sizeof(result));
  return result;
}

static void flag(const char *name, int *first) {
  printf("%s\"%s\"", *first ? "" : ",", name);
  *first = 0;
}

static void flags(int value) {
  int first = 1;
  if (value & FE_INVALID)
    flag("invalid", &first);
  if (value & FE_DIVBYZERO)
    flag("divide_by_zero", &first);
  if (value & FE_OVERFLOW)
    flag("overflow", &first);
  if (value & FE_UNDERFLOW)
    flag("underflow", &first);
  if (value & FE_INEXACT)
    flag("inexact", &first);
}

int main(int argc, char **argv) {
  if (argc != 3)
    return 2;
  volatile double input = from_bits(argv[2]);
  feclearexcept(FE_ALL_EXCEPT);
  errno = 123;
  volatile double result;
  if (strcmp(argv[1], "sqrt") == 0)
    result = sqrt(input);
  else if (strcmp(argv[1], "log") == 0)
    result = log(input);
  else if (strcmp(argv[1], "exp") == 0)
    result = exp(input);
  else
    return 2;
  int raised = fetestexcept(FE_ALL_EXCEPT);
  printf("{\"kind\":\"effects\",\"result_bits\":\"0x%016llx\",\"flags\":[",
         (unsigned long long)bits(result));
  flags(raised);
  printf("],\"errno\":%d,\"events\":[\"call\",\"report\"],\"trapped\":false}\n",
         errno);
  return 0;
}
