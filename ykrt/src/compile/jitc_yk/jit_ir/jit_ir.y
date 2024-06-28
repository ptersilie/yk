%start Module
%expect-unused Unmatched "UNMATCHED"

%%

Module -> Result<(Vec<ASTFuncDecl>, Vec<()>, Vec<ASTBBlock>), Box<dyn Error>>:
    FuncDecls Globals BBlocks {
      Ok(($1?, $2?, $3?))
    }
  ;

FuncDecls -> Result<Vec<ASTFuncDecl>, Box<dyn Error>>:
    FuncDecl FuncDecls { flatten($1, $2) }
  | { Ok(Vec::new()) }
  ;

FuncDecl -> Result<ASTFuncDecl, Box<dyn Error>>:
    "FUNC_DECL" "ID" "(" FuncArgs ")" FuncRtnType {
      let (arg_tys, is_varargs) = $4?;
      Ok(ASTFuncDecl{name: $2?.span(), arg_tys, is_varargs, rtn_ty: $6?})
    }
  ;

FuncArgs -> Result<(Vec<ASTType>, bool), Box<dyn Error>>:
    NormalFuncArgs "," "..." { Ok(($1?, true)) }
  | NormalFuncArgs "," Type { Ok((flattenr($1, $3)?, false)) }
  | Type { Ok((vec![$1?], false)) }
  | "..." { Ok((Vec::new(), true)) }
  | { Ok((Vec::new(), false)) }
  ;

NormalFuncArgs -> Result<Vec<ASTType>, Box<dyn Error>>:
    NormalFuncArgs "," Type { flattenr($1, $3) }
  | Type { Ok(vec![$1?]) }
  ;

FuncRtnType -> Result<ASTType, Box<dyn Error>>:
    "->" Type { $2 }
  | { Ok(ASTType::Void) }
  ;

Globals -> Result<Vec<()>, Box<dyn Error>>:
    "GLOBAL" Globals { todo!() }
  | { Ok(Vec::new()) }
  ;

BBlocks -> Result<Vec<ASTBBlock>, Box<dyn Error>>:
    BBlock BBlocks { flatten($1, $2) }
  | { Ok(Vec::new()) }
  ;

BBlock -> Result<ASTBBlock, Box<dyn Error>>:
    "LABEL" Insts { Ok(ASTBBlock{ label: $1?.span(), insts: $2?}) }
  ;

Insts -> Result<Vec<ASTInst>, Box<dyn Error>>:
    Inst Insts { flatten($1, $2) }
  | { Ok(Vec::new()) }
  ;

Inst -> Result<ASTInst, Box<dyn Error>>:
    "*" Operand "=" Operand { Ok(ASTInst::Store{tgt: $2?, val: $4?, volatile: false}) }
  | "*" Operand "=" Operand "," "VOLATILE" { Ok(ASTInst::Store{tgt: $2?, val: $4?, volatile: true}) }
  | "BLACK_BOX" Operand { Ok(ASTInst::BlackBox($2?)) }
  | "GUARD" "TRUE" "," Operand {
      Ok(ASTInst::Guard{operand: $4?, is_true: true})
    }
  | "GUARD" "FALSE" "," Operand {
      Ok(ASTInst::Guard{operand: $4?, is_true: false})
    }
  | "LOCAL_OPERAND" ":" Type "=" "LOAD_TI" "UINT" {
      Ok(ASTInst::LoadTraceInput{assign: $1?.span(), type_: $3?, off: $6?.span()})
    }
  | "LOCAL_OPERAND" ":" Type "=" BinOp Operand "," Operand  {
      Ok(ASTInst::BinOp{assign: $1?.span(), type_: $3?, bin_op: $5?, lhs: $6?, rhs: $8?})
    }
  | "LOCAL_OPERAND" ":" Type "=" "CALL" "GLOBAL" "(" CallArgs ")" {
      Ok(ASTInst::Call{assign: Some($1?.span()), name: $6?.span(), args: $8?})
    }
  | "CALL" "GLOBAL" "(" CallArgs ")" {
      Ok(ASTInst::Call{assign: None, name: $2?.span(), args: $4?})
    }
  | "LOCAL_OPERAND" ":" Type "=" Predicate Operand "," Operand  {
      Ok(ASTInst::ICmp{assign: $1?.span(), type_: $3?, pred: $5?, lhs: $6?, rhs: $8?})
    }
  | "LOCAL_OPERAND" ":" Type "=" "LOAD" Operand {
      Ok(ASTInst::Load{assign: $1?.span(), type_: $3?, val: $6?, volatile: false})
    }
  | "LOCAL_OPERAND" ":" Type "=" "LOAD" Operand "," "VOLATILE" {
      Ok(ASTInst::Load{assign: $1?.span(), type_: $3?, val: $6?, volatile: true})
    }
  | "LOCAL_OPERAND" ":" Type "=" "PTR_ADD" Operand "," "UINT" {
      Ok(ASTInst::PtrAdd{assign: $1?.span(), type_: $3?, ptr: $6?, off: $8?.span()})
    }
  | "LOCAL_OPERAND" ":" Type "=" "DYN_PTR_ADD" Operand "," Operand "," "UINT" {
      Ok(ASTInst::DynPtrAdd{assign: $1?.span(), type_: $3?, ptr: $6?, num_elems: $8?, elem_size: $10?.span()})
    }
  | "LOCAL_OPERAND" ":" Type "=" "SEXT" Operand {
      Ok(ASTInst::SExt{assign: $1?.span(), type_: $3?, val: $6? })
    }
  | "LOCAL_OPERAND" ":" Type "=" "SI_TO_FP" Operand {
      Ok(ASTInst::SIToFP{assign: $1?.span(), type_: $3?, val: $6? })
    }
  | "LOCAL_OPERAND" ":" Type "=" "FP_EXT" Operand {
      Ok(ASTInst::FPExt{assign: $1?.span(), type_: $3?, val: $6? })
    }
  | "LOCAL_OPERAND" ":" Type "=" "TRUNC" Operand {
      Ok(ASTInst::Trunc{assign: $1?.span(), type_: $3?, operand: $6? })
    }
  | "LOCAL_OPERAND" ":" Type "=" Operand "?" Operand ":" Operand {
      Ok(ASTInst::Select{assign: $1?.span(), cond: $5?, trueval: $7?, falseval: $9? })
    }
  | "LOCAL_OPERAND" ":" Type "=" Operand {
      Ok(ASTInst::Proxy{assign: $1?.span(), val: $5? })
    }
  | "TLOOP_START" { Ok(ASTInst::TraceLoopStart) }
  ;

Operand -> Result<ASTOperand, Box<dyn Error>>:
    "LOCAL_OPERAND" { Ok(ASTOperand::Local($1?.span())) }
  | "CONST_INT" { Ok(ASTOperand::ConstInt($1?.span())) }
  | "CONST_PTR" { Ok(ASTOperand::ConstPtr($1?.span())) }
  ;

Type -> Result<ASTType, Box<dyn Error>>:
    "INT_TYPE" { Ok(ASTType::Int($1?.span())) }
  | "FLOAT_TYPE" { Ok(ASTType::Float($1?.span())) }
  | "DOUBLE_TYPE" { Ok(ASTType::Double($1?.span())) }
  | "PTR" { Ok(ASTType::Ptr) }
  ;

BinOp -> Result<BinOp, Box<dyn Error>>:
    "ADD" { Ok(BinOp::Add) }
  | "SUB" { Ok(BinOp::Sub) }
  | "MUL" { Ok(BinOp::Mul) }
  | "OR" { Ok(BinOp::Or) }
  | "AND" { Ok(BinOp::And) }
  | "XOR" { Ok(BinOp::Xor) }
  | "SHL" { Ok(BinOp::Shl) }
  | "ASHR" { Ok(BinOp::AShr) }
  | "FADD" { Ok(BinOp::FAdd) }
  | "FDIV" { Ok(BinOp::FDiv) }
  | "FMUL" { Ok(BinOp::FMul) }
  | "FREM" { Ok(BinOp::FRem) }
  | "FSUB" { Ok(BinOp::FSub) }
  | "LSHR" { Ok(BinOp::LShr) }
  | "SDIV" { Ok(BinOp::SDiv) }
  | "SREM" { Ok(BinOp::SRem) }
  | "UDIV" { Ok(BinOp::UDiv) }
  | "UREM" { Ok(BinOp::URem) }
  ;

Predicate -> Result<Predicate, Box<dyn Error>>:
    "EQ" { Ok(Predicate::Equal) }
  | "NE" { Ok(Predicate::NotEqual) }
  | "UGT" { Ok(Predicate::UnsignedGreater) }
  | "UGE" { Ok(Predicate::UnsignedGreaterEqual) }
  | "ULT" { Ok(Predicate::UnsignedLess) }
  | "ULE" { Ok(Predicate::UnsignedLessEqual) }
  | "SGT" { Ok(Predicate::SignedGreater) }
  | "SGE" { Ok(Predicate::SignedGreaterEqual) }
  | "SLT" { Ok(Predicate::SignedLess) }
  | "SLE" { Ok(Predicate::SignedLessEqual) }
  ;

CallArgs -> Result<Vec<ASTOperand>, Box<dyn Error>>:
    CallArgs "," Operand { flattenr($1, $3) }
  | Operand { Ok(vec![$1?]) }
  | { Ok(Vec::new()) }
  ;

Unmatched -> ():
    "UNMATCHED" { }
  ;

%%

use crate::compile::jitc_yk::jit_ir::parser::*;
use std::error::Error;

fn flatten<T>(lhs: Result<T, Box<dyn Error>>, rhs: Result<Vec<T>, Box<dyn Error>>)
  -> Result<Vec<T>, Box<dyn Error>>
{
    let lhs = lhs?;
    let mut rhs = rhs?;
    let mut out = Vec::with_capacity(rhs.len() + 1);
    out.push(lhs);
    out.append(&mut rhs);
    Ok(out)
}

fn flattenr<T>(lhs: Result<Vec<T>, Box<dyn Error>>, rhs: Result<T, Box<dyn Error>>)
  -> Result<Vec<T>, Box<dyn Error>>
{
    let mut lhs = lhs?;
    let rhs = rhs?;
    lhs.push(rhs);
    Ok(lhs)
}
