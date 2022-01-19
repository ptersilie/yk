// Compiler:
//   env-var: YKD_PRINT_JITSTATE=1
// Run-time:
//   env-var: YKD_PRINT_IR=jit-pre-opt
//   env-var: YKD_SERIALISE_COMPILATION=1
//   stderr:
//     ...
//     i=25
//     jit-state: stopgap
//     ...

// Check that tracing mutation of a global pointer works.

#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <yk.h>
#include <yk_testing.h>

char *p = NULL;

int main(int argc, char **argv) {
  yk_set_hot_threshold(0);
  int i = 0;
  YkLocation loc = yk_location_new();
  p = argv[0];
  NOOPT_VAL(i);
  while (*p != '\0') {
    yk_control_point(&loc);
    fprintf(stderr, "i=%d\n", i);
    i++;
    p++;
  }
  abort(); // FIXME: unreachable due to aborting guard failure earlier.
  NOOPT_VAL(i);
  NOOPT_VAL(p);
  yk_location_drop(loc);

  return (EXIT_SUCCESS);
}