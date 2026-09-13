#include <math.h>

#if defined(PROBE_DISABLED_BUILTIN)
double probe(double value) { return sqrt(value); }
#elif defined(PROBE_USER_FUNCTION)
double sin(double value) { return value + 1.0; }
double probe(double value) { return sin(value); }
#elif defined(PROBE_INDIRECT_CALL)
static double (*volatile selected)(double) = sqrt;
double probe(double value) { return selected(value); }
#elif defined(PROBE_NESTED_SCOPE)
#pragma GCC push_options
#pragma GCC optimize("fp-contract=off")
double strict_scope(double a, double b, double c) { return a * b + c; }
#pragma GCC pop_options
double relaxed_scope(double a, double b, double c) { return a * b + c; }
#elif defined(PROBE_MIXED_INLINE)
__attribute__((optimize("fp-contract=off"))) static inline double
strict_inline(double a, double b, double c) {
  return a * b + c;
}
double mixed_inline(double a, double b, double c, double x, double y, double z) {
  return strict_inline(a, b, c) + x * y + z;
}
#else
#error missing recognition probe
#endif
