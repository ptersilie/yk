// Compiler:
// Run-time:
//   env-var: YKD_PRINT_IR=jit-pre-opt
//   stderr:
//     ...
//     ...call i32 @call_me(...
//     ...
//     declare i32 @call_me(i32)
//     ...

// Check that we can call a static function with internal linkage from the same
// compilation unit.

#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <yk_testing.h>

static int call_me(int x) {
  if (x == 5)
    return x;
  else {
    // The recursion will cause a call to be emitted in the trace.
    return call_me(x + 1);
  }
}

int main(int argc, char **argv) {
  int res = 0;
  void *tt = __yktrace_start_tracing(HW_TRACING, &res);
  NOOPT_VAL(argc);
  res = call_me(argc);
  NOOPT_VAL(res);
  void *tr = __yktrace_stop_tracing(tt);
  assert(res == 5);

  void *ptr = __yktrace_irtrace_compile(tr);
  __yktrace_drop_irtrace(tr);
  void (*func)(int *) = (void (*)(int *))ptr;
  int res2 = 0;
  func(&res2);
  assert(res2 == 5);

  return (EXIT_SUCCESS);
}
