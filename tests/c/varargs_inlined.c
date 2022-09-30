// Run-time:
//   env-var: YKD_PRINT_IR=aot
//   env-var: YKD_SERIALISE_COMPILATION=1
//   env-var: YKD_PRINT_JITSTATE=1
//   stderr:
//     jit-state: start-tracing
//     i=1
//     jit-state: stop-tracing
//     --- Begin aot ---
//     ...
//     call void @llvm.va_start...
//     ...
//     call void @llvm.va_end...
//     ...
//     --- End aot ---
//     i=1
//     jit-state: enter-jit-code
//     i=1
//     jit-state: exit-jit-code
//     jit-state: enter-jit-code
//     i=1
//     jit-state: enter-stopgap
//     jit-state: exit-stopgap

// Check that basic trace compilation works.

#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <yk.h>
#include <yk_testing.h>

int varargfunc(int len, ...) {
  int acc = 0;
  va_list argp;
  va_start(argp, len);
  int arg = va_arg(argp, int);
  acc += arg;
  va_end(argp);
  return acc;
}

int foo(int argc) { return varargfunc(3, argc, 2, 3); }

int main(int argc, char **argv) {
  YkMT *mt = yk_mt_new();
  yk_mt_hot_threshold_set(mt, 0);
  YkLocation loc = yk_location_new();

  int i = 4;
  NOOPT_VAL(loc);
  NOOPT_VAL(i);
  while (i > 0) {
    yk_mt_control_point(mt, &loc);
    int res = foo(argc);
    fprintf(stderr, "i=%d\n", res);
    i--;
  }
  yk_location_drop(loc);
  yk_mt_drop(mt);
  return (EXIT_SUCCESS);
}