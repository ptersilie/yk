use llvm_sys::bit_reader::LLVMParseBitcodeInContext2;
use llvm_sys::core::*;
use llvm_sys::prelude::{LLVMModuleRef, LLVMValueRef};
use llvm_sys::{LLVMOpcode};
use std::collections::HashMap;
use std::ffi::CStr;
use std::mem::MaybeUninit;

mod llvmbridge;
use llvmbridge::{LLVMModule, LLVMInst, LLVMBranchInst, LLVMRetInst, LLVMUser, LLVMValue, LLVMValueTrait};

/// Stopgap interpreter values.
#[derive(Debug)]
pub enum SGValue {
    U32(u32),
    U64(u64),
}

/// A frame holding live variables.
struct Frame {
    vars: HashMap<LLVMInst, SGValue>,
}

impl Frame {
    fn new() -> Frame {
        Frame {
            vars: HashMap::new(),
        }
    }

    /// Get the value of the variable `key` in this frame.
    fn get(&self, key: &LLVMInst) -> Option<&SGValue> {
        self.vars.get(key)
    }

    /// Insert new variable into this frame.
    fn add(&mut self, key: LLVMInst, val: SGValue) {
        self.vars.insert(key, val);
    }
}

/// The stopgap interpreter. Used during guard failures to get back to the control point by
/// interpreting LLVM IR.
pub struct SGInterp {
    /// LLVM IR module we are interpreting.
    module: LLVMModule,
    /// Current frames.
    frames: Vec<Frame>,
    /// Current instruction being interpreted.
    pc: LLVMInst,
}

impl SGInterp {
    /// Create a new stopgap interpreter and initialise it to start interpretation at the location
    /// given by a basic block index, instruction index, and function name.
    /// FIXME: Support initalisation of multiple frames.
    pub unsafe fn new(bbidx: u32, instridx: u32, fname: &CStr) -> SGInterp {
        // Get AOT module IR and parse it.
        let module = LLVMModule::from_bc();
        // Create and initialise stop gap interpreter.
        let func = module.function(fname.as_ptr());
        let bb = func.bb(bbidx);
        let instr = bb.instruction(instridx);
        SGInterp {
            module,
            frames: vec![Frame::new()],
            pc: instr,
        }
    }

    /// Add a live variable and its value to the current frame.
    pub unsafe fn init_live(&mut self, bbidx: u32, instridx: u32, fname: &CStr, value: SGValue) {
        let func = self.module.function(fname.as_ptr());
        let bb = func.bb(bbidx);
        let instr = bb.instruction(instridx);
        self.frames.last_mut().unwrap().add(instr, value);
    }

    /// Lookup the value of variable `var` in the current frame.
    fn lookup(&self, var: &LLVMInst) -> Option<&SGValue> {
        self.frames.last().unwrap().get(var)
    }

    /// Start interpretation of the initialised interpreter.
    pub unsafe fn interpret(&mut self) {
        // We start interpretation at the branch instruction that was turned into a guard. We need
        // to re-interpret this instruction in order to find out which branch we need to follow.
        loop {
            match self.pc.opcode() {
                LLVMOpcode::LLVMBr => self.branch(),
                LLVMOpcode::LLVMRet => self.ret(),
                _ => todo!("{:?}", self.pc.as_str()),
            }
        }
    }

    /// Interpret branch instruction `instr`.
    pub unsafe fn branch(&mut self) {
        let br = LLVMBranchInst::new(self.pc.valueref());
        let cond = br.condition();
        let cinst = LLVMInst::new(cond.valueref());
        let val = self.lookup(&cinst);
        let res = match val.unwrap() {
            SGValue::U32(v) => *v == 1,
            SGValue::U64(v) => *v == 1,
        };
        let succ = if res {
            br.successor(0)
        } else {
            br.successor(1)
        };
        self.pc = succ.first();
    }

    /// Interpret return instruction `instr`.
    unsafe fn ret(&mut self) {
        let ret = LLVMRetInst::new(self.pc.valueref());
        if self.frames.len() == 1 {
            // We've reached the end of the interpreters main, so just get the return value and
            // exit. This is possibly a hack, though I'm not sure what the correct behaviour is.
            let op = ret.get_operand(0);
            let val = if op.is_constant() {
                llvmbridge::llvm_const_to_sgvalue(op.valueref())
            } else {
                todo!()
            };
            let ret = match val {
                SGValue::U32(v) => v as i32,
                SGValue::U64(v) => v as i32,
            };
            std::process::exit(ret);
        }
    }
}
