#![feature(test)]
extern crate test;

use std::collections::HashMap;

use yktrace::tir::{TirTrace, TirOp, Statement, PlaceBase, Rvalue, Operand, Constant, ConstantInt,
UnsignedInt};

use assembler::{ExecutableAnonymousMemoryMap, InstructionStreamHints, InstructionStream};
use assembler::mnemonic_parameter_types::registers::*;
use assembler::mnemonic_parameter_types::immediates::*;

/// A compiled SIRTrace.
pub struct CompiledTrace {
    // We need to store both the function pointer as well as the memory map in this struct. We need to store
    // the memory map to hold on to it's data. Unfortunately, we don't have direct access to the
    // memory map's address from which the function pointer is generated, so we need to store the
    // function pointer as well. This means we never actually need to use the memory map reference.

    /// Pointer to the function that executes this trace.
    func: unsafe extern "C" fn() -> u64,
    /// Memory map where the compiled trace lives.
    _mmap: ExecutableAnonymousMemoryMap
}

impl CompiledTrace {
    pub fn execute(&self) -> u64 {
        // For now a compiled trace always returns whatever has been left in register RAX. We also
        // assume for now that this will be a u64.
        unsafe {
            (self.func)()
        }
    }
}

/// The TraceCompiler takes a SIRTrace and compiles it to machine code. Returns a CompiledTrace.
pub struct TraceCompiler<'a> {
    is: InstructionStream<'a>,
    available_regs: Vec<Register64Bit>,
    assigned_regs: HashMap<u32, Register64Bit>
}

impl TraceCompiler<'_> {

    fn local_to_reg(&mut self, l: u32) -> Register64Bit {
        // This is a really dumb register allocator, which runs out of available registers after 7
        // locals. We can do better than this by using StorageLive/StorageDead from the MIR to free
        // up registers again, and allocate additional locals on the stack. Though, ultimately we
        // probably want to implement a proper register allocator, e.g. graph colouring or linear
        // scan.
        match self.assigned_regs.get(&l) {
            Some(u) => {
                *u
            },
            None => {
                if l == 0 {
                    // Local 0 always maps to the return register RAX
                    Register64Bit::RAX
                }
                else {
                    let reg = self.available_regs
                        .pop()
                        .expect("Can't allocate more than 7 locals yet!");
                    self.assigned_regs.insert(l, reg);
                    reg
                }
            }
        }
    }

    pub fn local_usize(&mut self, a: u32, c: usize) {
        let reg = self.local_to_reg(a);
        self.is.mov_Register64Bit_Immediate64Bit(reg, Immediate64Bit::from(c as u64));
    }

    pub fn local_u8(&mut self, a: u32, c: u8) {
        let reg = self.local_to_reg(a);
        self.is.mov_Register64Bit_Immediate64Bit(reg, Immediate64Bit::from(c as u64));
    }

    fn local_local(&mut self, var1: u32, var2: u32) {
        let lreg = self.local_to_reg(var1);
        let rreg = self.local_to_reg(var2);
        self.is.cmova_Register64Bit_Register64Bit(lreg, rreg);
    }

    fn statement(&mut self, stmt: &Statement) {
        match stmt {
            Statement::Assign(l, r) => {
                let local = match l.base {
                    PlaceBase::Local(l) => l.0,
                    PlaceBase::Static => panic!("Not implemented: Static")
                };
                match r {
                    Rvalue::Use(Operand::Place(p)) => {
                        match p.base {
                            PlaceBase::Local(l) => self.local_local(local, l.0),
                            PlaceBase::Static => panic!("Not implemented: Static")
                        }
                    },
                    Rvalue::Use(Operand::Constant(c)) => {
                        match c {
                            Constant::Int(ci) => {
                                match ci {
                                    ConstantInt::UnsignedInt(UnsignedInt::U8(i)) => {
                                        self.local_u8(local, *i)
                                    },
                                    ConstantInt::UnsignedInt(UnsignedInt::Usize(i)) => {
                                        self.local_usize(local, *i)
                                    },
                                    e => panic!("SignedInt, etc: {}", e)
                                }
                            }
                            _ => panic!("Not implemented: int")
                        }
                    }
                    unimpl => panic!("Not implemented: {:?}", unimpl)
                };
            },
            Statement::Return => {},
            Statement::Nop => {},
            Statement::Unimplemented(mir_stmt) => println!("Can't compile: {}", mir_stmt)
        }
    }

    fn finish(mut self) {
        self.is.ret();
        self.is.finish();
    }

    pub fn compile(tt: TirTrace) -> CompiledTrace {
        dbg!(&tt);

        let mut regs = Vec::new();
        regs.push(Register64Bit::R11);
        regs.push(Register64Bit::R10);
        regs.push(Register64Bit::R9);
        regs.push(Register64Bit::R8);
        regs.push(Register64Bit::RCX);
        regs.push(Register64Bit::RDX);

        let mut memory_map = ExecutableAnonymousMemoryMap::new(4096, true, true).unwrap();
        let is = memory_map.instruction_stream(&InstructionStreamHints::default());
        let func: unsafe extern "C" fn () -> u64 = is.nullary_function_pointer();

        let mut tc = TraceCompiler {
            is: is,
            available_regs: regs,
            assigned_regs: HashMap::new()
        };

        for i in 0..tt.len()-1 {
            let t = tt.op(i);
            match t {
                TirOp::Statement(st) => tc.statement(st),
                TirOp::Guard(_) => { println!("Not implemented: Guard") }
            }
        }
        tc.finish();
        CompiledTrace{ func, _mmap: memory_map }
    }
}

#[cfg(test)]
mod tests {

    use super::TraceCompiler;
    use yktrace::{start_tracing, TracingKind};
    use yktrace::tir::TirTrace;

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
}
