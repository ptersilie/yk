use llvm_sys::core::*;
use llvm_sys::prelude::{LLVMModuleRef, LLVMBasicBlockRef, LLVMValueRef};
use llvm_sys::{LLVMTypeKind};
use llvm_sys::{LLVMOpcode};
use llvm_sys::bit_reader::LLVMParseBitcodeInContext2;
use std::mem::MaybeUninit;
use crate::SGValue;
use std::ffi::CStr;

pub struct LLVMModule(LLVMModuleRef);

impl LLVMModule {
    pub unsafe fn from_bc() -> Self {
        let (addr, size) = ykutil::obj::llvmbc_section();
        let membuf = LLVMCreateMemoryBufferWithMemoryRange(
            addr as *const i8,
            size,
            "".as_ptr() as *const i8,
            0,
        );
        let context = LLVMContextCreate();
        let mut module: MaybeUninit<LLVMModuleRef> = MaybeUninit::uninit();
        LLVMParseBitcodeInContext2(context, membuf, module.as_mut_ptr());
        let module = module.assume_init();
        Self(module)
    }

    pub unsafe fn function(&self, name: *const i8) -> LLVMFunction {
        let func = LLVMGetNamedFunction(self.0, name);
        debug_assert!(!func.is_null());
        LLVMFunction::new(func)
    }
}

pub struct LLVMFunction(LLVMValueRef);

impl LLVMFunction {
    pub unsafe fn new(func: LLVMValueRef) -> Self {
        debug_assert!(!LLVMIsAFunction(func).is_null());
        Self(func)
    }

    pub unsafe fn bb(&self, bbidx: u32) -> LLVMBasicBlock {
        let mut bb = LLVMGetFirstBasicBlock(self.0);
        for _ in 0..bbidx {
            bb = LLVMGetNextBasicBlock(bb);
        }
        LLVMBasicBlock::new(bb)
    }
}

pub struct LLVMBasicBlock(LLVMBasicBlockRef);

impl LLVMBasicBlock {
    pub unsafe fn new(bb: LLVMBasicBlockRef) -> Self {
        Self(bb)
    }

    pub unsafe fn first(&self) -> LLVMInst {
        self.instruction(0)
    }

    pub unsafe fn instruction(&self, instridx: u32) -> LLVMInst {
        let mut instr = LLVMGetFirstInstruction(self.0);
        for _ in 0..instridx {
            instr = LLVMGetNextInstruction(instr);
        }
        LLVMInst::new(instr)
    }
}

pub trait LLVMUser {
    unsafe fn get_operand(&self, idx: usize) -> LLVMValue {
        let op = LLVMGetOperand(self.valueref(), 0);
        LLVMValue(op)
    }

    unsafe fn is_constant(&self) -> bool {
        !LLVMIsAConstant(self.valueref()).is_null()
    }

    fn valueref(&self) -> LLVMValueRef;
}

#[derive(PartialEq, Eq, Hash)]
pub struct LLVMValue(LLVMValueRef);
impl LLVMUser for LLVMValue {
    fn valueref(&self) -> LLVMValueRef {
        self.0
    }
}

#[derive(PartialEq, Eq, Hash)]
pub struct LLVMInst(LLVMValueRef);

impl LLVMInst {
    pub unsafe fn new(instr: LLVMValueRef) -> Self {
        debug_assert!(!LLVMIsAInstruction(instr).is_null());
        Self(instr)
    }

    pub unsafe fn as_str(&self) -> &CStr {
        CStr::from_ptr(LLVMPrintValueToString(self.0))
    }

    pub unsafe fn opcode(&self) -> LLVMOpcode {
        LLVMGetInstructionOpcode(self.0)
    }

    pub unsafe fn valueref(&self) -> LLVMValueRef {
        self.0
    }
}

#[derive(PartialEq, Eq, Hash)]
pub struct LLVMBranchInst(LLVMValueRef);

impl LLVMBranchInst {
    pub unsafe fn new(instr: LLVMValueRef) -> Self {
        Self(instr)
    }

    pub unsafe fn condition(&self) -> LLVMValue {
        let cond = LLVMGetCondition(self.0);
        LLVMValue(cond)
    }

    pub unsafe fn successor(&self, idx: u32) -> LLVMBasicBlock {
        LLVMBasicBlock::new(LLVMGetSuccessor(self.0, idx))
    }
}


#[derive(PartialEq, Eq, Hash)]
pub struct LLVMRetInst(LLVMValueRef);
impl LLVMRetInst {
    pub unsafe fn new(instr: LLVMValueRef) -> Self {
        Self(instr)
    }
}
impl LLVMUser for LLVMRetInst {
    fn valueref(&self) -> LLVMValueRef {
        self.0
    }
}

pub unsafe fn llvm_const_to_sgvalue(c: LLVMValueRef) -> SGValue {
    let ty = LLVMTypeOf(c);
    let kind = LLVMGetTypeKind(ty);
    match kind {
        LLVMTypeKind::LLVMIntegerTypeKind => {
            let width = LLVMGetIntTypeWidth(ty);
            let val = LLVMConstIntGetZExtValue(c) as u64;
            match width {
                32 => SGValue::U32(val as u32),
                64 => SGValue::U64(val),
                _ => todo!(),
            }
        }
        _ => todo!(),
    }
}
