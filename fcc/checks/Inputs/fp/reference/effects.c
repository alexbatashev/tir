#define _GNU_SOURCE
#pragma STDC FENV_ACCESS ON

#include <errno.h>
#include <fenv.h>
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>

static sigjmp_buf jump;
static volatile sig_atomic_t event_state;
static volatile sig_atomic_t state_at_trap;

static void handle_fpe(int signal) {
  (void)signal;
  state_at_trap = event_state;
  event_state = 2;
  siglongjmp(jump, 1);
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

static int dead_division(void) {
  volatile double one = 1.0;
  volatile double zero = 0.0;
  feclearexcept(FE_ALL_EXCEPT);
  errno = 123;
  double result = one / zero;
  (void)result;
  int raised = fetestexcept(FE_ALL_EXCEPT);
  printf("{\"kind\":\"effects\",\"flags\":[");
  flags(raised);
  printf("],\"errno\":%d,\"events\":[\"clear\",\"divide\",\"test\"],\"trapped\":false}\n",
         errno);
  return 0;
}

static int trap_order(void) {
  struct sigaction action = {.sa_handler = handle_fpe};
  sigemptyset(&action.sa_mask);
  if (sigaction(SIGFPE, &action, NULL) != 0)
    return 3;
  volatile double one = 1.0;
  volatile double zero = 0.0;
  feclearexcept(FE_ALL_EXCEPT);
  event_state = 1;
  state_at_trap = 0;
  if (sigsetjmp(jump, 1) == 0) {
    if (feenableexcept(FE_DIVBYZERO) == -1)
      return 4;
    double result = one / zero;
    (void)result;
    event_state = 3;
  }
  fedisableexcept(FE_DIVBYZERO);
  printf("{\"kind\":\"effects\",\"flags\":[],\"errno\":null,\"events\":[\"store_before\"");
  if (state_at_trap == 3)
    printf(",\"store_after\",\"trap\"");
  else if (event_state == 2)
    printf(",\"trap\"");
  else if (event_state == 3)
    printf(",\"store_after\"");
  printf("],\"trapped\":%s}\n", event_state == 2 ? "true" : "false");
  return 0;
}

int main(int argc, char **argv) {
  if (argc != 2)
    return 2;
  if (strcmp(argv[1], "dead") == 0)
    return dead_division();
  if (strcmp(argv[1], "trap") == 0)
    return trap_order();
  return 2;
}
