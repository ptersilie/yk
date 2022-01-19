use llvm_sys::core::*;
use llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};
use llvm_sys::{LLVMTypeKind};
use crate::SGValue;

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

    pub unsafe fn instruction(&self, instridx: u32) -> LLVMValueRef {
        let mut instr = LLVMGetFirstInstruction(self.0);
        for _ in 0..instridx {
            instr = LLVMGetNextInstruction(instr);
        }
        instr
    }
}

#[derive(PartialEq, Eq, Hash)]
pub struct LLVMInstruction(LLVMValueRef);

impl LLVMInstruction {
    pub unsafe fn new(instr: LLVMValueRef) -> Self {
        debug_assert!(!LLVMIsAInstruction(instr).is_null());
        Self(instr)
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
