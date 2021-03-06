#![feature(proc_macro_hygiene)]
#![feature(test)]
#![feature(core_intrinsics)]

#[macro_use]
extern crate dynasm;
extern crate dynasmrt;
extern crate test;

use libc::{c_void, dlsym, RTLD_DEFAULT};
use std::collections::HashMap;
use std::fmt::{self, Display, Formatter};
use std::mem;
use std::process::Command;

use yktrace::tir::{
    CallOperand, Constant, ConstantInt, Guard, Local, Operand, Place, Rvalue, Statement, TirOp,
    TirTrace, UnsignedInt,
};

use dynasmrt::DynasmApi;

#[derive(Debug, Hash, Eq, PartialEq)]
pub enum CompileError {
    /// We ran out of registers.
    /// In the long-run, when we have a proper register allocator, this won't be needed.
    OutOfRegisters,
    /// Compiling this statement is not yet implemented.
    /// The string inside is a hint as to what kind of statement needs to be implemented.
    Unimplemented(String),
}

impl Display for CompileError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfRegisters => write!(f, "Ran out of registers"),
            Self::Unimplemented(s) => write!(f, "Unimplemented compilation: {}", s),
        }
    }
}

/// Converts a register number into it's string name.
fn reg_num_to_name(r: u8) -> &'static str {
    match r {
        0 => "rax",
        1 => "rcx",
        2 => "rdx",
        3 => "rbx",
        4 => "rsp",
        5 => "rbp",
        6 => "rsi",
        7 => "rdi",
        8 => "r8",
        9 => "r9",
        10 => "r10",
        11 => "r11",
        12 => "r12",
        13 => "r13",
        14 => "r14",
        15 => "r15",
        _ => unimplemented!(),
    }
}

/// A compiled SIRTrace.
pub struct CompiledTrace {
    /// A compiled trace.
    mc: dynasmrt::ExecutableBuffer,
}

impl CompiledTrace {
    pub fn execute(&self) -> u64 {
        // For now a compiled trace always returns whatever has been left in register RAX. We also
        // assume for now that this will be a `u64`.
        let func: fn() -> u64 = unsafe { mem::transmute(self.mc.ptr(dynasmrt::AssemblyOffset(0))) };
        func()
    }
}

/// The `TraceCompiler` takes a `SIRTrace` and compiles it to machine code. Returns a `CompiledTrace`.
pub struct TraceCompiler {
    asm: dynasmrt::x64::Assembler,
    /// Contains the list of currently available registers.
    available_regs: Vec<u8>,
    /// Maps locals to their assigned registers.
    assigned_regs: HashMap<Local, u8>,
    /// Stores the destination locals to which we copy RAX to after leaving an inlined call.
    leaves: Vec<Option<Place>>,
}

impl TraceCompiler {
    fn local_to_reg(&mut self, l: Local) -> Result<u8, CompileError> {
        // This is a really dumb register allocator, which runs out of available registers after 7
        // locals. We can do better than this by using StorageLive/StorageDead from the MIR to free
        // up registers again, and allocate additional locals on the stack. Though, ultimately we
        // probably want to implement a proper register allocator, e.g. linear scan.

        if l == Local(0) {
            // In SIR, `Local` zero is the (implicit) return value, so it makes sense to allocate
            // it to the return register of the underlying X86_64 calling convention.
            Ok(0)
        } else {
            if self.assigned_regs.contains_key(&l) {
                Ok(self.assigned_regs[&l])
            } else {
                if let Some(reg) = self.available_regs.pop() {
                    self.assigned_regs.insert(l, reg);
                    Ok(reg)
                } else {
                    Err(CompileError::OutOfRegisters)
                }
            }
        }
    }

    fn free_register(&mut self, local: &Local) {
        if let Some(reg) = self.assigned_regs.remove(local) {
            self.available_regs.push(reg);
        }
    }

    /// Move constant `c` of type `usize` into local `a`.
    pub fn mov_local_usize(&mut self, local: Local, cnst: usize) -> Result<(), CompileError> {
        let reg = self.local_to_reg(local)?;
        dynasm!(self.asm
            ; mov Rq(reg), cnst as i32
        );
        Ok(())
    }

    /// Move constant `c` of type `u8` into local `a`.
    pub fn mov_local_u8(&mut self, local: Local, cnst: u8) -> Result<(), CompileError> {
        let reg = self.local_to_reg(local)?;
        dynasm!(self.asm
            ; mov Rq(reg), cnst as i32
        );
        Ok(())
    }

    /// Move local `var2` into local `var1`.
    fn mov_local_local(&mut self, l1: Local, l2: Local) -> Result<(), CompileError> {
        let lreg = self.local_to_reg(l1)?;
        let rreg = self.local_to_reg(l2)?;
        dynasm!(self.asm
            ; mov Rq(lreg), Rq(rreg)
        );
        Ok(())
    }

    fn nop(&mut self) {
        dynasm!(self.asm
            ; nop
        );
    }

    fn c_mov_int(&mut self, local: Local, constant: &ConstantInt) -> Result<(), CompileError> {
        let reg = self.local_to_reg(local)?;
        let val = match constant {
            ConstantInt::UnsignedInt(UnsignedInt::U8(i)) => *i as i64,
            ConstantInt::UnsignedInt(UnsignedInt::Usize(i)) => *i as i64,
            e => return Err(CompileError::Unimplemented(format!("{}", e))),
        };
        dynasm!(self.asm
            ; mov Rq(reg), QWORD val
        );
        Ok(())
    }

    fn c_mov_bool(&mut self, local: Local, b: bool) -> Result<(), CompileError> {
        let reg = self.local_to_reg(local)?;
        dynasm!(self.asm
            ; mov Rq(reg), QWORD b as i64
        );
        Ok(())
    }

    fn c_enter(
        &mut self,
        op: &CallOperand,
        args: &Vec<Operand>,
        dest: &Option<Place>,
        off: u32,
    ) -> Result<(), CompileError> {
        // FIXME Currently, we still get a call to `stop_tracing` here, since the call is part of
        // the last block in the trace. We may be able to always skip the last n instructions of the
        // trace, but this requires some looking into to make sure we don't accidentally skip other
        // things. So for now, let's just skip the call here to get the tests working.
        match op {
            ykpack::CallOperand::Fn(s) => {
                if s.contains("stop_tracing") {
                    return Ok(());
                }
            }
            ykpack::CallOperand::Unknown => {}
        };
        // Move call arguments into registers.
        for (op, i) in args.iter().zip(1..) {
            let arg_idx = Local(i + off);
            match op {
                Operand::Place(p) => self.mov_local_local(arg_idx, p.local)?,
                Operand::Constant(c) => match c {
                    Constant::Int(ci) => self.c_mov_int(arg_idx, ci)?,
                    Constant::Bool(b) => self.c_mov_bool(arg_idx, *b)?,
                    c => return Err(CompileError::Unimplemented(format!("{}", c))),
                },
            }
        }
        // Remember the return destination.
        self.leaves.push(dest.as_ref().cloned());
        Ok(())
    }

    fn c_leave(&mut self) -> Result<(), CompileError> {
        let dest = self.leaves.pop();
        if let Some(d) = dest {
            if let Some(d) = d {
                // When we see a leave statement move whatever's left in RAX into the destination
                // local.
                self.mov_local_local(d.local, Local(0))?;
            }
        }
        Ok(())
    }

    fn statement(&mut self, stmt: &Statement) -> Result<(), CompileError> {
        match stmt {
            Statement::Assign(l, r) => {
                if !l.projection.is_empty() {
                    return Err(CompileError::Unimplemented(format!("{}", l)));
                }
                match r {
                    Rvalue::Use(Operand::Place(p)) => {
                        if !p.projection.is_empty() {
                            return Err(CompileError::Unimplemented(format!("{}", r)));
                        }
                        self.mov_local_local(l.local, p.local)?;
                    }
                    Rvalue::Use(Operand::Constant(c)) => match c {
                        Constant::Int(ci) => self.c_mov_int(l.local, ci)?,
                        Constant::Bool(b) => self.c_mov_bool(l.local, *b)?,
                        c => return Err(CompileError::Unimplemented(format!("{}", c))),
                    },
                    unimpl => return Err(CompileError::Unimplemented(format!("{}", unimpl))),
                };
            }
            Statement::Enter(op, args, dest, off) => self.c_enter(op, args, dest, *off)?,
            Statement::Leave => self.c_leave()?,
            Statement::StorageLive(_) => {}
            Statement::StorageDead(l) => self.free_register(l),
            c @ Statement::Call(..) => return Err(CompileError::Unimplemented(format!("{:?}", c))),
            Statement::Nop => {}
            Statement::Unimplemented(s) => {
                return Err(CompileError::Unimplemented(format!("{:?}", s)))
            }
        }

        Ok(())
    }

    fn guard(&mut self, _grd: &Guard) -> Result<(), CompileError> {
        self.nop(); // FIXME compile guards
        Ok(())
    }

    fn crash_dump(self, e: CompileError) -> ! {
        eprintln!("\nThe trace compiler crashed!\n");
        eprintln!("Reason: {}.\n", e);

        // To help us figure out what has gone wrong, we can print the disassembled instruction
        // stream with the help of `rasm2`.
        eprintln!("Executable code buffer:");
        let code = &*self.asm.finalize().unwrap();
        if code.is_empty() {
            eprintln!("  <empty buffer>");
        } else {
            let hex_code = hex::encode(code);
            let res = Command::new("rasm2")
                .arg("-d")
                .arg("-b 64") // x86_64.
                .arg(hex_code.clone())
                .output()
                .unwrap();
            if !res.status.success() {
                eprintln!("  Failed to invoke rasm2. Raw bytes follow...");
                eprintln!("  {}", hex_code);
            } else {
                let asm = String::from_utf8(res.stdout).unwrap();
                for line in asm.lines() {
                    eprintln!("  {}", line);
                }
            }
        }

        // Print the register allocation.
        eprintln!("\nRegister allocation (local -> reg):");
        for (local, reg) in &self.assigned_regs {
            eprintln!("  {:2} -> {:3} ({})", local, reg, reg_num_to_name(*reg));
        }
        eprintln!();

        panic!("stopped due to trace compilation error");
    }

    fn ret(&mut self) {
        dynasm!(self.asm
            ; ret
        );
    }

    fn finish(self) -> dynasmrt::ExecutableBuffer {
        self.asm.finalize().unwrap()
    }

    pub fn compile(tt: TirTrace) -> CompiledTrace {
        let assembler = dynasmrt::x64::Assembler::new().unwrap();

        let mut tc = TraceCompiler {
            asm: assembler,
            // Use all the 64-bit registers we can (R15-R8, RDX, RCX).
            available_regs: vec![15, 14, 13, 12, 11, 10, 9, 8, 2, 1],
            assigned_regs: HashMap::new(),
            leaves: Vec::new(),
        };

        for i in 0..tt.len() {
            let res = match tt.op(i) {
                TirOp::Statement(st) => tc.statement(st),
                TirOp::Guard(g) => tc.guard(g),
            };

            if let Err(e) = res {
                tc.crash_dump(e);
            }
        }

        tc.ret();
        CompiledTrace { mc: tc.finish() }
    }

    #[allow(dead_code)] // Not used just yet.
    fn find_symbol(sym: &str) -> Option<*mut c_void> {
        use std::ffi::CString;

        let sym_arg = CString::new(sym).unwrap();
        let addr = unsafe { dlsym(RTLD_DEFAULT, sym_arg.into_raw()) };

        if addr == 0 as *mut c_void {
            None
        } else {
            Some(addr)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{HashMap, Local, TraceCompiler};
    use libc::c_void;
    use std::collections::HashSet;
    use yktrace::tir::{CallOperand, Statement, TirOp, TirTrace};
    use yktrace::{start_tracing, TracingKind};

    #[inline(never)]
    fn simple() -> u8 {
        let x = 13;
        x
    }

    #[test]
    pub(crate) fn test_simple() {
        let th = start_tracing(Some(TracingKind::HardwareTracing));
        simple();
        let sir_trace = th.stop_tracing().unwrap();
        let tir_trace = TirTrace::new(&*sir_trace).unwrap();
        let ct = TraceCompiler::compile(tir_trace);
        assert_eq!(ct.execute(), 13);
    }

    // Repeatedly fetching the register for the same local should yield the same register and
    // should not exhaust the allocator.
    #[test]
    pub fn reg_alloc_same_local() {
        let mut tc = TraceCompiler {
            asm: dynasmrt::x64::Assembler::new().unwrap(),
            available_regs: vec![15, 14, 13, 12, 11, 10, 9, 8, 2, 1],
            assigned_regs: HashMap::new(),
            leaves: Vec::new(),
        };

        for _ in 0..32 {
            assert_eq!(
                tc.local_to_reg(Local(1)).unwrap(),
                tc.local_to_reg(Local(1)).unwrap()
            );
        }
    }

    // Locals should be allocated to different registers.
    #[test]
    pub fn reg_alloc() {
        let mut tc = TraceCompiler {
            asm: dynasmrt::x64::Assembler::new().unwrap(),
            available_regs: vec![15, 14, 13, 12, 11, 10, 9, 8, 2, 1],
            assigned_regs: HashMap::new(),
            leaves: Vec::new(),
        };

        let mut seen = HashSet::new();
        for l in 0..7 {
            let reg = tc.local_to_reg(Local(l));
            assert!(!seen.contains(&reg));
            seen.insert(reg);
        }
    }

    #[inline(never)]
    fn farg(i: u8) -> u8 {
        i
    }

    #[inline(never)]
    fn fcall() -> u8 {
        let y = farg(13); // assigns 13 to $1
        let _z = farg(14); // overwrites $1 within the call
        y // returns $1
    }

    #[test]
    pub(crate) fn test_function_call() {
        let th = start_tracing(Some(TracingKind::HardwareTracing));
        fcall();
        let sir_trace = th.stop_tracing().unwrap();
        let tir_trace = TirTrace::new(&*sir_trace).unwrap();
        let ct = TraceCompiler::compile(tir_trace);
        assert_eq!(ct.execute(), 13);
    }

    fn fnested3(i: u8, j: u8) -> u8 {
        let c = i;
        c
    }

    fn fnested2(i: u8) -> u8 {
        fnested3(i, 10)
    }

    fn fnested() -> u8 {
        let a = fnested2(20);
        a
    }

    #[test]
    pub(crate) fn test_function_call_nested() {
        let th = start_tracing(Some(TracingKind::HardwareTracing));
        fnested();
        let sir_trace = th.stop_tracing().unwrap();
        let tir_trace = TirTrace::new(&*sir_trace).unwrap();
        let ct = TraceCompiler::compile(tir_trace);
        assert_eq!(ct.execute(), 20);
    }

    // Test finding a symbol in a shared object.
    #[test]
    fn find_symbol_shared() {
        assert!(TraceCompiler::find_symbol("printf") == Some(libc::printf as *mut c_void));
    }

    // Test finding a symbol in the main binary.
    // For this to work the binary must have been linked with `--export-dynamic`, which ykrustc
    // appends to the linker command line.
    #[test]
    #[no_mangle]
    fn find_symbol_main() {
        assert!(
            TraceCompiler::find_symbol("find_symbol_main") == Some(find_symbol_main as *mut c_void)
        );
    }

    // Check that a non-existent symbol cannot be found.
    #[test]
    fn find_nonexistent_symbol() {
        assert!(TraceCompiler::find_symbol("__xxxyyyzzz__").is_none());
    }

    // A trace which contains a call to something which we don't have SIR for should emit a TIR
    // call operation.
    #[test]
    pub fn call_symbol() {
        let th = start_tracing(Some(TracingKind::HardwareTracing));
        let g = core::intrinsics::wrapping_add(10u64, 40u64);
        let sir_trace = th.stop_tracing().unwrap();
        let tir_trace = TirTrace::new(&*sir_trace).unwrap();

        let mut found_call = false;
        for i in 0..tir_trace.len() {
            if let TirOp::Statement(Statement::Call(CallOperand::Fn(sym), ..)) = tir_trace.op(i) {
                if sym.contains("wrapping_add") {
                    found_call = true;
                }
                break;
            }
        }
        assert!(found_call);
    }
}
