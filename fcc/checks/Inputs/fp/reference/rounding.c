#pragma STDC FENV_ACCESS ON

#include <errno.h>
#include <fenv.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static uint64_t parse_bits(const char *text) { return strtoull(text, NULL, 0); }

static double from_bits(const char *text) {
  uint64_t value = parse_bits(text);
  double result;
  memcpy(&result, &value, sizeof(result));
  return result;
}

static uint64_t double_bits(double value) {
  uint64_t result;
  memcpy(&result, &value, sizeof(result));
  return result;
}

static uint32_t float_bits(float value) {
  uint32_t result;
  memcpy(&result, &value, sizeof(result));
  return result;
}

static int rounding(const char *name) {
  if (strcmp(name, "nearest") == 0)
    return FE_TONEAREST;
  if (strcmp(name, "downward") == 0)
    return FE_DOWNWARD;
  if (strcmp(name, "upward") == 0)
    return FE_UPWARD;
  if (strcmp(name, "toward_zero") == 0)
    return FE_TOWARDZERO;
  return -1;
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
  if (argc < 4 || fesetround(rounding(argv[2])) != 0)
    return 2;
  feclearexcept(FE_ALL_EXCEPT);
  errno = 123;
  if (strcmp(argv[1], "add64") == 0 && argc == 5) {
    volatile double left = from_bits(argv[3]);
    volatile double right = from_bits(argv[4]);
    volatile double result = left + right;
    int raised = fetestexcept(FE_ALL_EXCEPT);
    printf("{\"kind\":\"exact_bits\",\"bits\":\"0x%016llx\",\"flags\":[",
           (unsigned long long)double_bits(result));
    flags(raised);
    printf("]}\n");
    return 0;
  }
  if (strcmp(argv[1], "mul64") == 0 && argc == 5) {
    volatile double left = from_bits(argv[3]);
    volatile double right = from_bits(argv[4]);
    volatile double result = left * right;
    int raised = fetestexcept(FE_ALL_EXCEPT);
    printf("{\"kind\":\"exact_bits\",\"bits\":\"0x%016llx\",\"flags\":[",
           (unsigned long long)double_bits(result));
    flags(raised);
    printf("]}\n");
    return 0;
  }
  if (strcmp(argv[1], "f64_to_f32") == 0 && argc == 4) {
    volatile double input = from_bits(argv[3]);
    volatile float result = (float)input;
    int raised = fetestexcept(FE_ALL_EXCEPT);
    printf("{\"kind\":\"exact_bits\",\"bits\":\"0x%08x\",\"flags\":[",
           float_bits(result));
    flags(raised);
    printf("]}\n");
    return 0;
  }
  if (strcmp(argv[1], "to_i64") == 0 && argc == 4) {
    volatile double input = from_bits(argv[3]);
    long long result = llrint(input);
    int raised = fetestexcept(FE_ALL_EXCEPT);
    printf("{\"kind\":\"exact_bits\",\"bits\":\"0x%016llx\",\"flags\":[",
           (unsigned long long)result);
    flags(raised);
    printf("]}\n");
    return 0;
  }
  if (strcmp(argv[1], "to_i64_invalid") == 0 && argc == 4) {
    volatile double input = from_bits(argv[3]);
    volatile long long result = llrint(input);
    (void)result;
    int raised = fetestexcept(FE_ALL_EXCEPT);
    printf("{\"kind\":\"effects\",\"flags\":[");
    flags(raised);
    printf("],\"errno\":%d,\"events\":[],\"trapped\":false}\n", errno);
    return 0;
  }
  return 2;
}
