// Classes and functions for constructing a new LLVM module from a trace.

#include "llvm/IR/DebugInfo.h"
#include "llvm/IR/Dominators.h"
#include "llvm/IR/IRBuilder.h"
#include "llvm/IR/Instructions.h"
#include "llvm/Transforms/Utils/ValueMapper.h"

#include "jitmodbuilder.h"

#include <atomic>
#include <err.h>

using namespace llvm;
using namespace std;

// An atomic counter used to issue compiled traces with unique names.
atomic<uint64_t> NextTraceIdx(0);
uint64_t getNewTraceIdx() {
  uint64_t TraceIdx = NextTraceIdx.fetch_add(1, memory_order_relaxed);
  assert(TraceIdx != numeric_limits<uint64_t>::max());
  return TraceIdx;
}

#define TRACE_FUNC_PREFIX "__yk_compiled_trace_"
#define YK_NEW_CONTROL_POINT "__ykrt_control_point"
#define YK_CONTROL_POINT_ARG_IDX 1

// Dump an error message and an LLVM value to stderr and exit with failure.
void dumpValueAndExit(const char *Msg, Value *V) {
  errs() << Msg << ": ";
  V->dump();
  exit(EXIT_FAILURE);
}

// A function name and basic block index pair that identifies a block in the
// AOT LLVM IR.
struct IRBlock {
  // A non-null pointer to the function name.
  char *FuncName;
  // The index of the block in the parent LLVM function.
  size_t BBIdx;
};

// Describes the software or hardware trace to be compiled using LLVM.
class InputTrace {
private:
  // An ordered array of function names. Each non-null element describes the
  // function part of a (function, block) pair that identifies an LLVM
  // BasicBlock. A null element represents unmappable code in the trace.
  char **FuncNames;
  // An ordered array of basic block indices. Each element corresponds with
  // an element (at the same index) in the above `FuncNames` array to make a
  // (function, block) pair that identifies an LLVM BasicBlock.
  size_t *BBs;
  // The length of the `FuncNames` and `BBs` arrays.
  size_t Len;

public:
  InputTrace(char **FuncNames, size_t *BBs, size_t Len)
      : FuncNames(FuncNames), BBs(BBs), Len(Len) {}
  size_t Length() { return Len; }

  // Returns the optional IRBlock at index `Idx` in the trace. No value is
  // returned if element at `Idx` was unmappable. It is undefined behaviour to
  // invoke this method with an out-of-bounds `Idx`.
  const Optional<IRBlock> operator[](size_t Idx) {
    assert(Idx < Len);
    char *FuncName = FuncNames[Idx];
    if (FuncName == nullptr) {
      return Optional<IRBlock>();
    } else {
      return Optional<IRBlock>(IRBlock{FuncName, BBs[Idx]});
    }
  }

  // The same as `operator[]`, but for scenarios where you are certain that the
  // element at position `Idx` cannot be unmappable.
  const IRBlock getUnchecked(size_t Idx) {
    assert(Idx < Len);
    char *FuncName = FuncNames[Idx];
    assert(FuncName != nullptr);
    return IRBlock{FuncName, BBs[Idx]};
  }
};

// Function virtual addresses observed in the input trace.
// Maps a function symbol name to a virtual address.
class FuncAddrs {
  map<string, void *> Map;

public:
  FuncAddrs(char **FuncNames, void **VAddrs, size_t Len) {
    for (size_t I = 0; I < Len; I++) {
      Map.insert({FuncNames[I], VAddrs[I]});
    }
  }

  // Lookup the address of the specified function name or return nullptr on
  // failure.
  void *operator[](const char *FuncName) {
    auto It = Map.find(FuncName);
    if (It == Map.end())
      return nullptr; // Not found.
    return It->second;
  }
};

/// Get the `Value` of the `YkCtrlPointVars` struct by looking it up inside the
/// arguments of the new control point.
Value *getYkCtrlPointVarsStruct(Module *AOTMod, InputTrace &InpTrace) {
  Function *F = AOTMod->getFunction(YK_NEW_CONTROL_POINT);
  assert(F != nullptr);
  User *CallSite = F->user_back();
  CallInst *CI = cast<CallInst>(CallSite);
  return CI->getArgOperand(YK_CONTROL_POINT_ARG_IDX);
}

/// Extract all live variables that need to be passed into the control point.
/// FIXME: This is currently an overapproximation and will return some
/// variables that are no longer alive.
std::vector<Value *> getLiveVars(DominatorTree &DT, Instruction *Before) {
  std::vector<Value *> Vec;
  Function *Func = Before->getFunction();
  for (auto &BB : *Func) {
    if (!DT.dominates(cast<Instruction>(Before), &BB)) {
      for (auto &I : BB) {
        if ((!I.getType()->isVoidTy()) &&
            (DT.dominates(&I, cast<Instruction>(Before)))) {
          Vec.push_back(&I);
        }
      }
    }
  }
  return Vec;
}

class JITModBuilder {
  // Global variables/functions that were copied over and need to be
  // initialised.
  vector<GlobalVariable *> cloned_globals;
  // The module being traced.
  Module *AOTMod;
  // The new module that is being build.
  Module *JITMod;
  // A pointer to the call to YK_NEW_CONTROL_POINT in the AOT module (once
  // encountered). When this changes from NULL to non-NULL, then we start
  // copying instructions from the AOT module into the JIT module.
  Instruction *NewControlPointCall = nullptr;
  // Stack of inlined calls, required to resume at the correct place in the
  // caller.
  std::vector<tuple<size_t, CallInst *>> InlinedCalls;
  // Instruction at which to continue after an a call.
  Optional<tuple<size_t, CallInst *>> ResumeAfter;
  // Depth of nested calls when outlining a recursive function.
  size_t RecCallDepth = 0;
  // Signifies a hole (for which we have no IR) in the trace.
  bool ExpectUnmappable = false;
  // The JITMod's builder.
  llvm::IRBuilder<> Builder;
  // Dead values to recursively delete upon finalisation of the JITMod. This is
  // required because it's not safe to recursively delete values in the middle
  // of creating the JIT module. We don't know if any of those values might be
  // required later in the trace.
  vector<Value *> DeleteDeadOnFinalise;

  // Information about the trace we are compiling.
  InputTrace InpTrace;
  // Function virtual addresses discovered from the input trace.
  FuncAddrs FAddrs;

  // A stack of BasicBlocks. Each time we enter a new call frame, we push the
  // first basic block to the stack. Following a branch to another basic block
  // updates the most recently pushed block. This is required for selecting the
  // correct incoming value when tracing a PHI node.
  vector<BasicBlock *> LastCompletedBlocks;

  // Maps field indices in the `YkCtrlPointVars` struct to the value stored
  // prior to calling the control point.
  std::map<uint64_t, Value *> LiveIndexMap;

  // The block we branch to when a guard fails. Starts null, lazily created.
  BasicBlock *GuardFailBB;

  // The LLVM type for a C `int` on the current machine.
  Type *IntTy;

  // Map JIT instruction to basic block index and instruction index of the
  // corresponding AOT instruction.
  std::map<Value *, std::tuple<size_t, size_t, Instruction *>> AOTMap;

  Value *getMappedValue(Value *V) {
    if (VMap.find(V) != VMap.end()) {
      return VMap[V];
    }
    assert(isa<Constant>(V));
    return V;
  }

  void insertAOTMap(Instruction *AOT, Value *JIT, size_t BBIdx,
                    size_t InstrIdx) {
    AOTMap[JIT] = {BBIdx, InstrIdx, AOT};
  }

  // Returns true if the given function exists on the call stack, which means
  // this is a recursive call.
  bool isRecursiveCall(Function *F) {
    for (auto Tup : InlinedCalls) {
      CallInst *CInst = get<1>(Tup);
      if (CInst->getCalledFunction() == F) {
        return true;
      }
    }
    return false;
  }

  // Add an external declaration for the given function to JITMod.
  void declareFunction(Function *F) {
    assert(JITMod->getFunction(F->getName()) == nullptr);
    auto DeclFunc = llvm::Function::Create(F->getFunctionType(),
                                           GlobalValue::ExternalLinkage,
                                           F->getName(), JITMod);
    VMap[F] = DeclFunc;
  }

  // Find the machine code corresponding to the given AOT IR function and
  // ensure there's a mapping from its name to that machine code.
  void addGlobalMappingForFunction(Function *CF) {
    StringRef CFName = CF->getName();
    void *FAddr = FAddrs[CFName.data()];
    assert(FAddr != nullptr);
    GlobalMappings.insert({CF, FAddr});
  }

  void handleCallInst(CallInst *CI, Function *CF, size_t &CurBBIdx,
                      size_t &CurInstrIdx) {
    if (CF == nullptr || CF->isDeclaration()) {
      // The definition of the callee is external to AOTMod. We still
      // need to declare it locally if we have not done so yet.
      if (CF != nullptr && VMap.find(CF) == VMap.end()) {
        declareFunction(CF);
      }
      if (RecCallDepth == 0) {
        copyInstruction(&Builder, (Instruction *)&*CI, CurBBIdx, CurInstrIdx);
      }
      // We should expect an "unmappable hole" in the trace. This is
      // where the trace followed a call into external code for which we
      // have no IR, and thus we cannot map blocks for.
      ExpectUnmappable = true;
      ResumeAfter = make_tuple(CurInstrIdx, CI);
    } else {
      LastCompletedBlocks.push_back(nullptr);
      if (RecCallDepth > 0) {
        // When outlining a recursive function, we need to count all other
        // function calls so we know when we left the recusion.
        RecCallDepth += 1;
        InlinedCalls.push_back(make_tuple(CurInstrIdx, CI));
        return;
      }
      // If this is a recursive call that has been inlined, remove the
      // inlined code and turn it into a normal call.
      if (isRecursiveCall(CF)) {
        if (VMap.find(CF) == VMap.end()) {
          declareFunction(CF);
          addGlobalMappingForFunction(CF);
        }
        copyInstruction(&Builder, CI, CurBBIdx, CurInstrIdx);
        InlinedCalls.push_back(make_tuple(CurInstrIdx, CI));
        RecCallDepth = 1;
        return;
      }
      // This is neither recursion nor an external call, so keep it inlined.
      InlinedCalls.push_back(make_tuple(CurInstrIdx, CI));
      // Remap function arguments to the variables passed in by the caller.
      for (unsigned int i = 0; i < CI->arg_size(); i++) {
        Value *Var = CI->getArgOperand(i);
        Value *Arg = CF->getArg(i);
        // Check the operand for things we need to remap, e.g. globals.
        handleOperand(Var);
        // If the operand has already been cloned into JITMod then we
        // need to use the cloned value in the VMap.
        VMap[Arg] = getMappedValue(Var);
      }
    }
  }

  // Emits a guard for a LLVM `br` instruction, returning a pointer to the
  // guard success block, or null if no guard was required.
  BasicBlock *handleBranchInst(Function *JITFunc, BasicBlock *NextBlock,
                               Instruction *I, size_t CurBBIdx,
                               size_t CurInstrIdx) {
    assert(isa<BranchInst>(I));
    BranchInst *BI = cast<BranchInst>(I);
    LLVMContext &Context = JITMod->getContext();

    if (BI->isUnconditional())
      return nullptr; // Control-flow can not diverge. No guard required.

    // A conditional branch should have two successors and one of them should
    // be the block we observed in the trace.
    assert(BI->getNumSuccessors() == 2);
    assert((BI->getSuccessor(0) == NextBlock) ||
           (BI->getSuccessor(1) == NextBlock));

    // Get/create the guard failure and success blocks.
    BasicBlock *FailBB = getGuardFailureBlock(JITFunc, CurBBIdx, CurInstrIdx,
                                              I->getFunction()->getName());
    BasicBlock *SuccBB = BasicBlock::Create(Context, "", JITFunc);

    // Insert the guard, using the original AOT branch condition for now.
    //
    // OPT: Could add branch weights to `CreateCondBr` to hint to LLVM that we
    // expect the guard to rarely fail?
    BranchInst *Guard = Builder.CreateCondBr(getMappedValue(BI->getCondition()),
                                             SuccBB, FailBB);

    // If the trace took the false arm of the AOT branch, then we have to
    // invert the condition of the guard we just inserted into the trace.
    if (BI->getSuccessor(0) != NextBlock)
      Guard->swapSuccessors();

    return SuccBB;
  }

  // Emits a guard for a LLVM `switch` instruction, returning a pointer to the
  // guard success block.
  BasicBlock *handleSwitchInst(Function *JITFunc, BasicBlock *NextBlock,
                               Instruction *I, size_t CurBBIdx,
                               size_t CurInstrIdx) {
    assert(isa<SwitchInst>(I));
    SwitchInst *SI = cast<SwitchInst>(I);

    // Get/create the guard failure and success blocks.
    LLVMContext &Context = JITMod->getContext();
    BasicBlock *FailBB = getGuardFailureBlock(JITFunc, CurBBIdx, CurInstrIdx,
                                              I->getFunction()->getName());
    BasicBlock *SuccBB = BasicBlock::Create(Context, "", JITFunc);

    // Determine which switch case the trace took.
    ConstantInt *MatchedValue = SI->findCaseDest(NextBlock);
    if (MatchedValue != nullptr) {
      // A non-default case was taken.
      Value *Cmp = Builder.CreateICmpEQ(getMappedValue(SI->getCondition()),
                                        MatchedValue);
      Builder.CreateCondBr(Cmp, SuccBB, FailBB);
    } else {
      // The default case was taken.
      SwitchInst *NewSI = Builder.CreateSwitch(
          getMappedValue(SI->getCondition()), SuccBB, SI->getNumCases());
      for (SwitchInst::CaseHandle &Case : SI->cases())
        NewSI->addCase(Case.getCaseValue(), FailBB);
    }

    return SuccBB;
  }

  void handleReturnInst(Instruction *I) {
    ResumeAfter = InlinedCalls.back();
    InlinedCalls.pop_back();
    LastCompletedBlocks.pop_back();
    if (RecCallDepth > 0) {
      RecCallDepth -= 1;
      return;
    }
    // Replace the return variable of the call with its return value.
    // Since the return value will have already been copied over to the
    // JITModule, make sure we look up the copy.
    auto OldRetVal = ((ReturnInst *)&*I)->getReturnValue();
    if (OldRetVal != nullptr) {
      assert(ResumeAfter.hasValue());
      VMap[get<1>(ResumeAfter.getValue())] = getMappedValue(OldRetVal);
    }
  }

  void handlePHINode(Instruction *I, BasicBlock *BB) {
    Value *V = ((PHINode *)&*I)->getIncomingValueForBlock(BB);
    VMap[&*I] = getMappedValue(V);
  }

  Function *createJITFunc(Value *TraceInputs, Type *RetTy) {
    // Compute a name for the trace.
    uint64_t TraceIdx = getNewTraceIdx();
    TraceName = string(TRACE_FUNC_PREFIX) + to_string(TraceIdx);

    // Create the function.
    std::vector<Type *> InputTypes;
    InputTypes.push_back(TraceInputs->getType());
// Add arguments for stackmap pointer and size.
#if defined(__x86_64)
    InputTypes.push_back(Type::getInt64PtrTy(RetTy->getContext()));
    InputTypes.push_back(Type::getInt64Ty(RetTy->getContext()));
#else
#error Not implemented!
#endif
    llvm::FunctionType *FType =
        llvm::FunctionType::get(RetTy, InputTypes, false);
    llvm::Function *JITFunc = llvm::Function::Create(
        FType, Function::InternalLinkage, TraceName, JITMod);
    JITFunc->setCallingConv(CallingConv::C);

    return JITFunc;
  }

  // Delete the dead value `V` from its parent, also deleting any dependencies
  // of `V` (i.e. operands) which then become dead.
  void deleteDeadTransitive(Value *V) {
    assert(V->user_empty()); // The value must be dead.
    vector<Value *> Work;
    Work.push_back(V);
    while (!Work.empty()) {
      Value *V = Work.back();
      Work.pop_back();
      // Remove `V` (an instruction or a global variable) from its parent
      // container. If any of the operands of `V` have a sole use, then they
      // will become dead and can also be deleted too.
      if (isa<Instruction>(V)) {
        Instruction *I = cast<Instruction>(V);
        for (auto &Op : I->operands()) {
          if (Op->hasOneUser()) {
            Work.push_back(&*Op);
          }
        }
        I->eraseFromParent();
      } else if (isa<GlobalVariable>(V)) {
        GlobalVariable *G = cast<GlobalVariable>(V);
        for (auto &Op : G->operands()) {
          if (Op->hasOneUser()) {
            Work.push_back(&*Op);
          }
        }
        // Be sure to remove this global variable from `cloned_globals` too, so
        // that we don't try to add an initialiser later in `finalise()`.
        erase_if(cloned_globals, [G, this](GlobalVariable *CG) {
          assert(VMap.find(CG) != VMap.end());
          return G == VMap[CG];
        });
        G->eraseFromParent();
      } else {
        dumpValueAndExit("Unexpected Value", V);
      }
    }
  }

  // Given an `IRBlock`, find and return the LLVM data structures for the basic
  // block and its parent function.
  std::pair<Function *, BasicBlock *> getLLVMAOTFuncAndBlock(IRBlock *IB) {
    Function *F = AOTMod->getFunction(IB->FuncName);
    assert(F != nullptr);

    // Skip to the correct block.
    auto It = F->begin();
    std::advance(It, IB->BBIdx);
    BasicBlock *BB = &*It;

    return {F, BB};
  }

  // Returns a pointer to the guard failure block, creating it if necessary.
  BasicBlock *getGuardFailureBlock(Function *JITFunc, size_t CurBBIdx,
                                   size_t CurInstrIdx, StringRef FuncName) {
    if (GuardFailBB == nullptr) {
      // If `JITFunc` contains no blocks already, then the guard failure block
      // becomes the entry block. This would lead to a trace that
      // unconditionally and immediately fails a guard.
      assert(JITFunc->getBasicBlockList().size() != 0);

      // Declare `errx(3)`.
      LLVMContext &Context = JITFunc->getContext();

      // Create the block.
      GuardFailBB = BasicBlock::Create(Context, "guardfail", JITFunc);
      IRBuilder<> FailBuilder(GuardFailBB);

      // Find live variables.
      BasicBlock *CurrentBB = Builder.GetInsertBlock();
      Instruction *CurrentInst = &CurrentBB->back();
      DominatorTree DT(*JITFunc);
      std::vector<Value *> LiveVals = getLiveVars(DT, CurrentInst);
      // Naturally the current instruction is live too but wasn't included due
      // to the way DominatorTree works.
      LiveVals.push_back(CurrentInst);

      // FIXME use more flexible type than int32
      IntegerType *Int32Ty = Type::getInt32Ty(Context);
      PointerType *Int8PtrTy = Type::getInt8PtrTy(Context);

      // Create struct storing current basic block index and instruction index.
      // This will be needed later to point the stopgap interpeter at the
      // correct location from where to start interpretation.
      // FIXME: Use function index instead of string name.
      StructType *CurPosSTy =
          StructType::get(Context, {Int32Ty, Int32Ty, Int8PtrTy});
      AllocaInst *CurPos =
          Builder.CreateAlloca(CurPosSTy, ConstantInt::get(Int32Ty, 1));
      auto GEP = Builder.CreateGEP(
          CurPosSTy, CurPos,
          {ConstantInt::get(Int32Ty, 0), ConstantInt::get(Int32Ty, 0)});
      Builder.CreateStore(ConstantInt::get(Int32Ty, CurBBIdx), GEP);
      GEP = Builder.CreateGEP(
          CurPosSTy, CurPos,
          {ConstantInt::get(Int32Ty, 0), ConstantInt::get(Int32Ty, 1)});
      Builder.CreateStore(ConstantInt::get(Int32Ty, CurInstrIdx), GEP);
      Value *CurFunc = Builder.CreateGlobalStringPtr(FuncName);
      GEP = Builder.CreateGEP(
          CurPosSTy, CurPos,
          {ConstantInt::get(Int32Ty, 0), ConstantInt::get(Int32Ty, 2)});
      Builder.CreateStore(CurFunc, GEP);

      // Create a vector in which to store the locations of the corresponding
      // AOT variables.
      StructType *AOTLocTy =
          StructType::get(Context, {Int32Ty, Int32Ty, Int8PtrTy});
      AllocaInst *AOTLocVec = Builder.CreateAlloca(
          AOTLocTy, ConstantInt::get(Int32Ty, LiveVals.size()));
      std::map<std::string, Value *> FuncPtrMap;
      for (size_t I = 0; I < LiveVals.size(); I++) {
        Value *Live = LiveVals[I];
        std::tuple<size_t, size_t, Instruction *> Entry = AOTMap[Live];
        size_t BBIdx = std::get<0>(Entry);
        size_t InstrIdx = std::get<1>(Entry);
        Instruction *AOTVar = std::get<2>(Entry);
        auto iter = FuncPtrMap.find(AOTVar->getFunction()->getName().data());
        Value *FPtr;
        if (iter == FuncPtrMap.end()) {
          // FIXME: Use function index instead of string name.
          FPtr =
              Builder.CreateGlobalStringPtr(AOTVar->getFunction()->getName());
          FuncPtrMap.insert({AOTVar->getFunction()->getName().data(), FPtr});
        } else {
          FPtr = iter->second;
        }
        auto GEP = Builder.CreateGEP(
            AOTLocTy, AOTLocVec,
            {ConstantInt::get(Int32Ty, I), ConstantInt::get(Int32Ty, 0)});
        Builder.CreateStore(ConstantInt::get(Int32Ty, BBIdx), GEP);
        GEP = Builder.CreateGEP(
            AOTLocTy, AOTLocVec,
            {ConstantInt::get(Int32Ty, I), ConstantInt::get(Int32Ty, 1)});
        Builder.CreateStore(ConstantInt::get(Int32Ty, InstrIdx), GEP);
        GEP = Builder.CreateGEP(
            AOTLocTy, AOTLocVec,
            {ConstantInt::get(Int32Ty, I), ConstantInt::get(Int32Ty, 2)});
        Builder.CreateStore(FPtr, GEP);
      }

      // Store the live variable vector and its length in a separate struct to
      // save arguments.
      PointerType *AOTLocVecPtrTy = PointerType::get(AOTLocTy, 0);
      StructType *AOTMapSTy =
          StructType::get(Context, {AOTLocVecPtrTy, Int32Ty});
      AllocaInst *AOTMap =
          Builder.CreateAlloca(AOTMapSTy, ConstantInt::get(Int32Ty, 1));
      GEP = Builder.CreateGEP(
          AOTMapSTy, AOTMap,
          {ConstantInt::get(Int32Ty, 0), ConstantInt::get(Int32Ty, 0)});
      Builder.CreateStore(AOTLocVec, GEP);
      GEP = Builder.CreateGEP(
          AOTMapSTy, AOTMap,
          {ConstantInt::get(Int32Ty, 0), ConstantInt::get(Int32Ty, 1)});
      Builder.CreateStore(ConstantInt::get(Int32Ty, LiveVals.size()), GEP);

      // Create the deoptimization call.
      Type *voidty = Type::getVoidTy(Context);
      Function *DeoptInt = Intrinsic::getDeclaration(
          JITFunc->getParent(), Intrinsic::experimental_deoptimize, {voidty});
      OperandBundleDef ob =
          OperandBundleDef("deopt", (ArrayRef<Value *>)LiveVals);
      // We already passed the stackmap address and size into the trace
      // function so pass them on to the __llvm_deoptimize call.
      CallInst::Create(DeoptInt,
                       {JITFunc->getArg(1), JITFunc->getArg(2), AOTMap, CurPos},
                       {ob}, "", GuardFailBB);

      // We always need to return after the deoptimisation call.
      ReturnInst::Create(Context, nullptr, GuardFailBB);
    }
    return GuardFailBB;
  }

  void handleBranchingControlFlow(Instruction *I, size_t TraceIdx,
                                  Function *JITFunc, size_t CurBBIdx,
                                  size_t CurInstrIdx) {
    // First, peek ahead in the trace and retrieve the next block. We need this
    // so that we can insert an appropriate guard into the trace. A block must
    // exist at `InpTrace[TraceIdx + 1]` because the branch instruction must
    // transfer to a successor block, and branching cannot turn off tracing.
    assert(InpTrace[TraceIdx + 1].hasValue()); // Should be a mappable block.
    IRBlock NextIB = InpTrace[TraceIdx + 1].getValue();
    BasicBlock *NextBB;
    Function *NextFunc;
    std::tie(NextFunc, NextBB) = getLLVMAOTFuncAndBlock(&NextIB);

    // The branching instructions we are handling here are all transfer to a
    // block in the same function.
    assert(NextFunc == I->getFunction());

    BasicBlock *SuccBB = nullptr;
    if (isa<BranchInst>(I)) {
      SuccBB = handleBranchInst(JITFunc, NextBB, &*I, CurBBIdx, CurInstrIdx);
    } else if (isa<SwitchInst>(I)) {
      SuccBB = handleSwitchInst(JITFunc, NextBB, &*I, CurBBIdx, CurInstrIdx);
    } else {
      assert(isa<IndirectBrInst>(I));
      // It isn't necessary to copy the indirect branch into the `JITMod`
      // as the successor block is known from the trace. However, naively
      // not copying the branch would lead to dangling references in the
      // IR because the `address` operand typically (indirectly)
      // references AOT block addresses not present in the `JITMod`.
      // Therefore we also remove the IR instruction which defines the
      // `address` operand and anything which also becomes dead as a
      // result (recursively).
      Value *FirstOp = I->getOperand(0);
      assert(VMap.find(FirstOp) != VMap.end());
      DeleteDeadOnFinalise.push_back(VMap[FirstOp]);
      // FIXME: guards for indirect branches are not yet implemented.
      // https://github.com/ykjit/yk/issues/438
      abort();
    }

    // If a guard was emitted, then the block we had been building the trace
    // into will have been terminated (to check the guard condition) and we
    // should resume building the trace into the new guard success block.
    if (SuccBB != nullptr)
      Builder.SetInsertPoint(SuccBB);
  }

  void handleOperand(Value *Op) {
    if (VMap.find(Op) == VMap.end()) {
      // The operand is undefined in JITMod.
      Type *OpTy = Op->getType();

      // Variables allocated outside of the traced section must be passed into
      // the trace and thus must already have a mapping.
      assert(!isa<llvm::AllocaInst>(Op));

      if (isa<ConstantExpr>(Op)) {
        // A `ConstantExpr` may contain operands that require remapping, e.g.
        // global variables. Iterate over all operands and recursively call
        // `handleOperand` on them, then generate a new `ConstantExpr` with
        // the remapped operands.
        ConstantExpr *CExpr = cast<ConstantExpr>(Op);
        std::vector<Constant *> NewCEOps;
        for (unsigned CEOpIdx = 0; CEOpIdx < CExpr->getNumOperands();
             CEOpIdx++) {
          Value *CEOp = CExpr->getOperand(CEOpIdx);
          handleOperand(CEOp);
          NewCEOps.push_back(cast<Constant>(getMappedValue(CEOp)));
        }
        Constant *NewCExpr = CExpr->getWithOperands(NewCEOps);
        VMap[CExpr] = NewCExpr;
      } else if (isa<GlobalVariable>(Op)) {
        // If there's a reference to a GlobalVariable, copy it over to the
        // new module.
        GlobalVariable *OldGV = cast<GlobalVariable>(Op);
        // Global variable is a constant so just copy it into the trace.
        // We don't need to check if this global already exists, since
        // we're skipping any operand that's already been cloned into
        // the VMap.
        GlobalVariable *GV = new GlobalVariable(
            *JITMod, OldGV->getValueType(), OldGV->isConstant(),
            OldGV->getLinkage(), (Constant *)nullptr, OldGV->getName(),
            (GlobalVariable *)nullptr, OldGV->getThreadLocalMode(),
            OldGV->getType()->getAddressSpace());
        VMap[OldGV] = GV;
        if (OldGV->isConstant()) {
          GV->copyAttributesFrom(&*OldGV);
          cloned_globals.push_back(OldGV);
        }
      } else if ((isa<Constant>(Op)) || (isa<InlineAsm>(Op))) {
        if (isa<Function>(Op)) {
          // We are storing a function pointer in a variable, so we need to
          // redeclare the function in the JITModule in case it gets called.
          declareFunction(cast<Function>(Op));
        }
        // Constants and inline asm don't need to be mapped.
      } else if (Op == NewControlPointCall) {
        // The value generated by NewControlPointCall is the thread tracer.
        // At some optimisation levels, this gets stored in an alloca'd
        // stack space. Since we've stripped the instruction that
        // generates that value (from the JIT module), we have to make a
        // dummy stack slot to keep LLVM happy.
        Value *NullVal = Constant::getNullValue(OpTy);
        VMap[Op] = NullVal;
      } else {
        dumpValueAndExit("don't know how to handle operand", Op);
      }
    }
  }

  void copyInstruction(IRBuilder<> *Builder, Instruction *I, size_t CurBBIdx,
                       size_t CurInstrIdx) {
    // Before copying an instruction, we have to scan the instruction's
    // operands checking that each is defined in JITMod.
    for (unsigned OpIdx = 0; OpIdx < I->getNumOperands(); OpIdx++) {
      Value *Op = I->getOperand(OpIdx);
      handleOperand(Op);
    }

    // Shortly we will copy the instruction into the JIT module. We start by
    // cloning the instruction.
    auto NewInst = &*I->clone();

    // Since the instruction operands still reference values from the AOT
    // module, we must remap them to point to new values in the JIT module.
    llvm::RemapInstruction(NewInst, VMap, RF_NoModuleLevelChanges);
    VMap[&*I] = NewInst;
    insertAOTMap(I, NewInst, CurBBIdx, CurInstrIdx);

    // Copy over any debugging metadata required by the instruction.
    llvm::SmallVector<std::pair<unsigned, llvm::MDNode *>, 1> metadataList;
    I->getAllMetadata(metadataList);
    for (auto MD : metadataList) {
      NewInst->setMetadata(
          MD.first,
          MapMetadata(MD.second, VMap, llvm::RF_ReuseAndMutateDistinctMDs));
    }

    // And finally insert the new instruction into the JIT module.
    Builder->Insert(NewInst);
  }

  // Finalise the JITModule by adding a return instruction and initialising
  // global variables.
  void finalise(Module *AOTMod, IRBuilder<> *Builder) {
    // Now that we've seen all possible uses of values in the JITMod, we can
    // delete the values we've marked dead (and possibly their dependencies if
    // they too turn out to be dead).
    for (auto &V : DeleteDeadOnFinalise)
      deleteDeadTransitive(V);

    // Fix initialisers/referrers for copied global variables.
    // FIXME Do we also need to copy Linkage, MetaData, Comdat?
    for (GlobalVariable *G : cloned_globals) {
      GlobalVariable *NewGV = cast<GlobalVariable>(VMap[G]);
      if (G->isDeclaration())
        continue;

      if (G->hasInitializer())
        NewGV->setInitializer(MapValue(G->getInitializer(), VMap));
    }

    // Ensure that the JITModule has a `!llvm.dbg.cu`.
    // This code is borrowed from LLVM's `cloneFunction()` implementation.
    // OPT: Is there a faster way than scanning the whole module?
    DebugInfoFinder DIFinder;
    DIFinder.processModule(*AOTMod);
    if (DIFinder.compile_unit_count()) {
      auto *NMD = JITMod->getOrInsertNamedMetadata("llvm.dbg.cu");
      SmallPtrSet<const void *, 8> Visited;
      for (auto *Operand : NMD->operands())
        Visited.insert(Operand);
      for (auto *Unit : DIFinder.compile_units())
        if (Visited.insert(Unit).second)
          NMD->addOperand(Unit);
    }
  }

  // Determines if the LLVM values `V1` and `V2` are instructions defined
  // within the same LLVM `BasicBlock`. `V1` and `V2` must both be an instance
  // of `Instruction`.
  bool areInstrsDefinedInSameBlock(Value *V1, Value *V2) {
    assert(isa<Instruction>(V1) && isa<Instruction>(V2));
    return cast<Instruction>(V1)->getParent() ==
           cast<Instruction>(V2)->getParent();
  }

  // When executing the interpreter loop AOT code, the code before the control
  // point is executed, then the control point is called, then the code after
  // the control point is executed.
  //
  // But when we collect a trace, the first code we see is the code *after* the
  // call to the control point, then (assuming the interpreter loop doesn't
  // exit) we branch back to the start of the loop and only then see the code
  // before the call to the control point.
  //
  // In other words, there is a disparity between the order of the code in the
  // AOT module and in collected traces and this has implications for the trace
  // compiler. Without extra logic, alloca'd variables become undefined (as
  // they are defined outside of the trace) and thus need to be remapped to the
  // input of the compiled trace. SSA values (from the same block as the
  // control point) remain correct as phi nodes at the beginning of the trace
  // automatically select the appropriate input value.
  //
  // For example, once patched, a typical interpreter loop will look like this:
  //
  // clang-format off
  //
  // ```
  // bb0:
  //   %a = alloca  // Stack variable
  //   store 0, %a
  //   %b = 1       // Register variable
  //   %s = alloca YkCtrlPointVars
  //   br %bb1
  //
  // bb1:
  //   %b1 = phi [%b, %bb0], [%binc, %bb1]
  //   %aptr = getelementptr %YkCtrlPointVars, %YkCtrlPointVars* %s, i32 0, i32 0
  //   store %aptr, %a
  //   %bptr = getelementptr %YkCtrlPointVars, %YkCtrlPointVars* %s, i32 0, i32 1
  //   store %bptr, %b
  //   // traces end here
  //   call yk_new_control_point(%s)
  //   // traces start here
  //   %aptr = getelementptr %YkCtrlPointVars, %YkCtrlPointVars* %s, i32 0, i32 0
  //   %anew = load %aptr
  //   %bptr = getelementptr %YkCtrlPointVars, %YkCtrlPointVars* %s, i32 0, i32 1
  //   %bnew = load %bptr
  //
  //   %aload = load %anew
  //   %ainc = add 1, %aload
  //   store %ainc, %a
  //   %binc = add 1, %bnew
  //   br %bb1
  // ```
  //
  // clang-format on
  //
  // There are two live variables stored into the `YKCtrlPointVars` struct
  // before the call to the control point (`%a` and `%b`), and those variables
  // are loaded back out after the call to the control point (into `%anew` and
  // `%bnew`). `%a` and `%anew` correspond to the same high-level variable, and
  // so do `%b1` and `%bnew`. When assembling a trace from the above IR, it
  // would look like this:
  //
  // clang-format off
  //
  // ```
  // void compiled_trace(%YkCtrlPointVars* %s) {
  //   %aptr = getelementptr %YkCtrlPointVars, %YkCtrlPointVars* %s, i32 0, i32 0
  //   %anew = load %aptr
  //   %bptr = getelementptr %YkCtrlPointVars, %YkCtrlPointVars* %s, i32 0, i32 1
  //   %bnew = load %bptr
  //
  //   %aload = load %anew
  //   %ainc = add 1, %aload
  //   store %ainc, %a                // %a is undefined
  //   %binc = add 1, %bnew
  //   %b1 = %bbinc                   // RHS selected from PHI.
  //
  //   %aptr = getelementptr %YkCtrlPointVars, %YkCtrlPointVars* %s, i32 0, i32 0
  //   store %aptr, %a
  //   %bptr = getelementptr %YkCtrlPointVars, %YkCtrlPointVars* %s, i32 0, i32 1
  //   store %bptr, %b1
  //   ...
  // }
  // ```
  //
  // clang-format on
  //
  // Here `%a` is undefined because we didn't trace its allocation. Instead we
  // need to use the definition extracted from the `YkCtrlPointVars`, which
  // means we need to replace `%a` with `%anew` in the store instruction. The
  // other value `%b` doesn't have this problem, since the PHI node in the
  // control point block already makes sure it selects the correct SSA value
  // `%binc`.
  void createLiveIndexMap(Instruction *CPCI, Type *YkCtrlPointVarsPtrTy) {
    BasicBlock *CPCIBB = CPCI->getParent();

    // Scan for `getelementpointer`/`store` pairs leading up the control point.
    // For each pair we add an entry to `LiveIndexMap`.
    //
    // For example, this instruction pair:
    //
    // ```
    // %19 = getelementptr %YkCtrlPointVars, %YkCtrlPointVars* %3, i32 0, i32 2
    // store i32* %6, i32** %19, align 8
    // ```
    //
    // Adds an entry mapping the index `2` to `%6`.
    for (BasicBlock::iterator CI = CPCIBB->begin(); &*CI != CPCI; CI++) {
      assert(CI != CPCIBB->end());
      if (!isa<GetElementPtrInst>(CI))
        continue;

      GetElementPtrInst *GI = cast<GetElementPtrInst>(CI);
      if (GI->getPointerOperandType() != YkCtrlPointVarsPtrTy)
        continue;

      // We have seen a lookup into the live variables struct, the succeeding
      // store instruction tells us which value is written into that field.
      Instruction *NextInst = &*std::next(CI);
      assert(isa<StoreInst>(NextInst));
      StoreInst *SI = cast<StoreInst>(NextInst);
      Value *StoredVal = SI->getValueOperand();
      Value *StoredAtIdxVal = *(std::next(GI->idx_begin()));
      assert(isa<ConstantInt>(StoredAtIdxVal));
      uint64_t StoredAtIdx = cast<ConstantInt>(StoredAtIdxVal)->getZExtValue();

      // We need an entry in this map for any live variable that isn't defined
      // by a PHI node at the top of the block cotaining the call to the
      // control point.
      if (!(isa<PHINode>(StoredVal) &&
            areInstrsDefinedInSameBlock(StoredVal, SI))) {
        LiveIndexMap[StoredAtIdx] = StoredVal;
      }
    }
  }

public:
  // Store virtual addresses for called functions.
  std::map<GlobalValue *, void *> GlobalMappings;
  // The function name of this trace.
  string TraceName;
  // Mapping from AOT instructions to JIT instructions.
  ValueToValueMapTy VMap;

  // OPT: https://github.com/ykjit/yk/issues/419
  JITModBuilder(Module *AOTMod, char *FuncNames[], size_t BBs[],
                size_t TraceLen, char *FAddrKeys[], void *FAddrVals[],
                size_t FAddrLen)
      : Builder(AOTMod->getContext()), InpTrace(FuncNames, BBs, TraceLen),
        FAddrs(FAddrKeys, FAddrVals, FAddrLen) {
    this->AOTMod = AOTMod;
    LLVMContext &Context = AOTMod->getContext();
    JITMod = new Module("", Context);
    GuardFailBB = nullptr;
    IntTy = Type::getIntNTy(Context, sizeof(int) * 8);
  }

  // Generate the JIT module.
  Module *createModule() {
    LLVMContext &JITContext = JITMod->getContext();
    // Find the trace inputs.
    Value *TraceInputs = getYkCtrlPointVarsStruct(AOTMod, InpTrace);

    // Get new control point call.
    Function *F = AOTMod->getFunction(YK_NEW_CONTROL_POINT);
    User *CallSite = F->user_back();
    CallInst *CPCI = cast<CallInst>(CallSite);
    Type *YkCtrlPointVarsPtrTy = F->getArg(1)->getType();
    assert(YkCtrlPointVarsPtrTy->isPointerTy());

    createLiveIndexMap(CPCI, YkCtrlPointVarsPtrTy);

    // Create function to store compiled trace.
    Function *JITFunc = createJITFunc(TraceInputs, CPCI->getType());

    // Map the YkCtrlPointVars struct used inside the trace to the argument of
    // the compiled trace function.
    VMap[TraceInputs] = JITFunc->getArg(0);

    // Create entry block and setup builder.
    auto DstBB = BasicBlock::Create(JITContext, "", JITFunc);
    Builder.SetInsertPoint(DstBB);

    LastCompletedBlocks.push_back(nullptr);
    BasicBlock *NextCompletedBlock = nullptr;

    // Iterate over the trace and stitch together all traced blocks.
    for (size_t Idx = 0; Idx < InpTrace.Length(); Idx++) {
      Optional<IRBlock> MaybeIB = InpTrace[Idx];
      if (ExpectUnmappable && !MaybeIB.hasValue()) {
        ExpectUnmappable = false;
        continue;
      }
      assert(MaybeIB.hasValue());
      IRBlock IB = MaybeIB.getValue();
      size_t CurBBIdx = IB.BBIdx;

      Function *F;
      BasicBlock *BB;
      std::tie(F, BB) = getLLVMAOTFuncAndBlock(&IB);

      assert(LastCompletedBlocks.size() >= 1);
      LastCompletedBlocks.back() = NextCompletedBlock;
      NextCompletedBlock = BB;

      // Iterate over all instructions within this block and copy them over
      // to our new module.
      for (size_t CurInstrIdx = 0; CurInstrIdx < BB->size(); CurInstrIdx++) {
        // If we've returned from a call, skip ahead to the instruction where
        // we left off.
        if (ResumeAfter.hasValue() != 0) {
          CurInstrIdx = std::get<0>(ResumeAfter.getValue()) + 1;
          ResumeAfter.reset();
        }
        auto I = BB->begin();
        std::advance(I, CurInstrIdx);
        assert(I != BB->end());

        // Skip calls to debug intrinsics (e.g. @llvm.dbg.value). We don't
        // currently handle debug info and these "pseudo-calls" cause our blocks
        // to be prematurely terminated.
        if (isa<DbgInfoIntrinsic>(I))
          continue;

        if (isa<CallInst>(I)) {

          if (isa<IntrinsicInst>(I)) {
            // All intrinsic calls must have metadata attached that specifies
            // whether it has been inlined or not.
            MDNode *IMD = I->getMetadata("yk.intrinsic.inlined");
            assert(IMD != nullptr);
            ConstantAsMetadata *CAM =
                cast<ConstantAsMetadata>(IMD->getOperand(0));
            if (CAM->getValue()->isOneValue()) {
              continue;
            }
          }

          CallInst *CI = cast<CallInst>(I);
          Function *CF = CI->getCalledFunction();
          if (CF == nullptr) {
            if (NewControlPointCall == nullptr) {
              continue;
            }
            // The target isn't statically known, so we can't inline the
            // callee.
            if (!isa<InlineAsm>(CI->getCalledOperand())) {
              // Look ahead in the trace to find the callee so we can
              // map the arguments if we are inlining the call.
              Optional<IRBlock> MaybeNextIB = InpTrace[Idx + 1];
              if (MaybeNextIB.hasValue()) {
                CF = AOTMod->getFunction(MaybeNextIB.getValue().FuncName);
              } else {
                CF = nullptr;
              }
              // FIXME Don't inline indirect calls unless promoted.
              handleCallInst(CI, CF, CurBBIdx, CurInstrIdx);
              break;
            }
          } else if (CF->getName() == YK_NEW_CONTROL_POINT) {
            ExpectUnmappable = true; // control point is always opaque.
            if (NewControlPointCall == nullptr) {
              NewControlPointCall = &*CI;
            } else {
              VMap[CI] = getMappedValue(CI->getArgOperand(1));
              ResumeAfter = make_tuple(CurInstrIdx, CI);
              break;
            }
            continue;
          } else if (NewControlPointCall != nullptr) {
            handleCallInst(CI, CF, CurBBIdx, CurInstrIdx);
            break;
          }
        }

        // We don't start copying instructions into the JIT module until we've
        // seen the call to YK_NEW_CONTROL_POINT.
        if (NewControlPointCall == nullptr)
          continue;

        if ((isa<BranchInst>(I)) || (isa<IndirectBrInst>(I)) ||
            (isa<SwitchInst>(I))) {
          handleBranchingControlFlow(&*I, Idx, JITFunc, CurBBIdx, CurInstrIdx);
          break;
        }

        if (isa<ReturnInst>(I)) {
          handleReturnInst(&*I);
          break;
        }

        if (RecCallDepth > 0) {
          // We are currently ignoring an inlined function.
          continue;
        }

        if (isa<PHINode>(I)) {
          assert(LastCompletedBlocks.size() >= 1);
          handlePHINode(&*I, LastCompletedBlocks.back());
          continue;
        }

        // If execution reaches here, then the instruction I is to be copied
        // into JITMod.
        copyInstruction(&Builder, (Instruction *)&*I, CurBBIdx, CurInstrIdx);

        // If we see a `getelementpointer`/`load` pair that is loading from the
        // `YkCtrlPointVars` pointer, then we have to update the `VMap` using
        // the information we previously computed in `LiveIndexMap`. See
        // comments above about `LiveIndexMap`.
        if (isa<LoadInst>(I)) {
          LoadInst *LI = cast<LoadInst>(I);
          Value *LoadOper = LI->getPointerOperand();
          if (isa<GetElementPtrInst>(LoadOper)) {
            GetElementPtrInst *GI = cast<GetElementPtrInst>(LoadOper);
            if (GI->getPointerOperandType() == YkCtrlPointVarsPtrTy) {
              Value *LoadedFromIdxVal = *(std::next(GI->idx_begin()));
              assert(isa<ConstantInt>(LoadedFromIdxVal));
              uint64_t LoadedFromIdx =
                  cast<ConstantInt>(LoadedFromIdxVal)->getZExtValue();
              Value *NewMapVal = LiveIndexMap[LoadedFromIdx];
              VMap[NewMapVal] = getMappedValue(LI);
            }
          }
        }
      }
    }

    Builder.CreateRetVoid();
    finalise(AOTMod, &Builder);
    return JITMod;
  }
};

tuple<Module *, string, std::map<GlobalValue *, void *>>
createModule(Module *AOTMod, char *FuncNames[], size_t BBs[], size_t TraceLen,
             char *FAddrKeys[], void *FAddrVals[], size_t FAddrLen) {
  JITModBuilder JB(AOTMod, FuncNames, BBs, TraceLen, FAddrKeys, FAddrVals,
                   FAddrLen);
  auto JITMod = JB.createModule();
  return make_tuple(JITMod, std::move(JB.TraceName),
                    std::move(JB.GlobalMappings));
}