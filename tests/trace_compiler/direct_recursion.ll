; Run-time:
;   env-var: YKD_PRINT_IR=jit-pre-opt
;   env-var: YKT_TRACE_BBS=main:0,f:0,f:1,f:0,f:1,f:0,f:2,f:1,f:2,f:1,f:2,main:0
;   stderr:
;     --- Begin jit-pre-opt ---
;
;     ...
;     define {{type}} @__yk_compiled_trace_0(ptr %0, ptr %1, i64 %2, ptr %3, ptr %4) {
;     entry:
;       %{{4}} = icmp eq i32 2, 0
;       br i1 %{{4}}, label %guardfail, label %{{5}}
;
;     guardfail:...
;       ...
;       %{{cprtn}} = call {{type}} (...) @llvm.experimental.deoptimize.p0(...
;       ret {{type}} %{{cprtn}}
;
;     {{5}}:...
;       %{{6}} = sub i32 2, 1
;       call void @f(i32 %{{6}})
;       ret {{type}}...
;     }
;
;     declare {{type}} @llvm.experimental.deoptimize.p0(...)
;
;     declare void @f(i32)
;     ...
;     --- End jit-pre-opt ---

define void @f(i32 %0) {
    %2 = icmp eq i32 %0, 0
    call void (i64, i32, ...) @llvm.experimental.stackmap(i64 1, i32 0, i1 %2)
    br i1 %2, label %done, label %recurse
recurse:
    %3 = sub i32 %0, 1
    call void @f(i32 %3)
    call void (i64, i32, ...) @llvm.experimental.stackmap(i64 2, i32 0, i32 %3)
    br label %done
done:
    ret void
}

define void @main() {
entry:
    call void @f(i32 2)
    call void (i64, i32, ...) @llvm.experimental.stackmap(i64 2, i32 0)
    ret void
}
declare void @llvm.experimental.stackmap(i64, i32, ...)
