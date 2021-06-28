// LLVM-related C++ code wrapped in the C ABI for calling from Rust.

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "llvm/ExecutionEngine/Orc/ThreadSafeModule.h"
#include <llvm/DebugInfo/Symbolize/Symbolize.h>
#include <llvm/ExecutionEngine/ExecutionEngine.h>
#include <llvm/ExecutionEngine/MCJIT.h>
#include <llvm/IR/IRBuilder.h>
#include <llvm/IR/LLVMContext.h>
#include <llvm/IR/Module.h>
#include <llvm/IR/Verifier.h>
#include <llvm/IRReader/IRReader.h>
#include <llvm/Support/SourceMgr.h>
#include <llvm/Support/TargetSelect.h>
#include <llvm/Transforms/Utils/ValueMapper.h>
#include <llvm/Transforms/Utils/Cloning.h>

#include <atomic>
#include <dlfcn.h>
#include <err.h>
#include <limits>
#include <link.h>
#include <mutex>
#include <stdlib.h>
#include <string.h>

#include "memman.cc"

using namespace llvm;
using namespace llvm::orc;
using namespace llvm::symbolize;
using namespace std;

extern "C" void __ykutil_get_llvmbc_section(void **res_addr, size_t *res_size);

// The bitcode module loaded from the .llvmbc section of the currently-running
// binary. This cannot be shared across threads and used concurrently without
// acquiring a lock, and since we do want to allow parallel compilation, each
// thread takes a copy of this module.
ThreadSafeModule GlobalAOTMod;

// Flag used to ensure that GlobalAOTMod is loaded only once.
once_flag GlobalAOTModLoaded;

// A copy of GlobalAOTMod for use by a single thread.
//
// A thread should never access this directly, but should instead go via
// getThreadAOTMod() which deals with the necessary lazy initialisation.
//
// PERF: Copying GlobalAOTMod is quite expensive (cloneToNewContext()
// serialises and deserializes). When a compilation thread dies, we should
// return its ThreadAOTMod to a pool and transfer ownership to the next thread
// that needs its own copy of GlobalAOTMod.
thread_local ThreadSafeModule ThreadAOTMod;

// A flag indicating whether GlobalAOTMod has been copied into the thread yet.
thread_local bool ThreadAOTModInitialized = false;

#define TRACE_FUNC_PREFIX "__yk_compiled_trace_"
#define YKTRACE_START "__yktrace_start_tracing"
#define YKTRACE_STOP "__yktrace_stop_tracing"

// An atomic counter used to issue compiled traces with unique names.
atomic<uint64_t> NextTraceIdx(0);

// Flag used to ensure that LLVM is initialised only once.
once_flag LLVMInitialised;

// Initialise LLVM for JIT compilation. This must be executed exactly once.
void initLLVM(void *Unused) {
  InitializeNativeTarget();
  InitializeNativeTargetAsmPrinter();
  InitializeNativeTargetAsmParser();
}

extern "C" LLVMSymbolizer *__yk_llvmwrap_symbolizer_new() {
  return new LLVMSymbolizer;
}

extern "C" void __yk_llvmwrap_symbolizer_free(LLVMSymbolizer *Symbolizer) {
  delete Symbolizer;
}

// Finds the name of a code symbol from a virtual address.
// The caller is responsible for freeing the returned (heap-allocated) C string.
extern "C" char *
__yk_llvmwrap_symbolizer_find_code_sym(LLVMSymbolizer *Symbolizer,
                                       const char *Obj, uint64_t Off) {
  object::SectionedAddress Mod{Off, object::SectionedAddress::UndefSection};
  auto LineInfo = Symbolizer->symbolizeCode(Obj, Mod);
  if (auto Err = LineInfo.takeError()) {
    return NULL;
  }

  // PERF: get rid of heap allocation.
  return strdup(LineInfo->FunctionName.c_str());
}

// Load the GlobalAOTMod.
//
// This must only be called from getAOTMod() for correct synchronisation.
void loadAOTMod(void *Unused) {
  void *SecPtr;
  size_t SecSize;
  __ykutil_get_llvmbc_section(&SecPtr, &SecSize);
  auto Sf = StringRef((const char *)SecPtr, SecSize);
  auto Mb = MemoryBufferRef(Sf, "");
  SMDiagnostic Error;
  ThreadSafeContext AOTCtx = std::make_unique<LLVMContext>();
  auto M = parseIR(Mb, Error, *AOTCtx.getContext());
  if (!M)
    errx(EXIT_FAILURE, "Can't load module.");
  GlobalAOTMod = ThreadSafeModule(std::move(M), std::move(AOTCtx));
}

// Get a thread-safe handle on the LLVM module stored in the .llvmbc section of
// the binary. The module is loaded if we haven't yet done so.
ThreadSafeModule *getThreadAOTMod(void) {
  std::call_once(GlobalAOTModLoaded, loadAOTMod, nullptr);
  if (!ThreadAOTModInitialized) {
    ThreadAOTMod = cloneToNewContext(GlobalAOTMod);
    ThreadAOTModInitialized = true;
  }
  return &ThreadAOTMod;
}

std::vector<Value *> get_trace_inputs(Function *F, uintptr_t BBIdx) {
  std::vector<Value *> Vec;
  auto It = F->begin();
  // Skip to the first block in the trace which contains the `start_tracing`
  // call.
  std::advance(It, BBIdx);
  BasicBlock *BB = &*It;
  for (auto I = BB->begin(); I != BB->end(); I++) {
    if (isa<CallInst>(I)) {
      CallInst *CI = cast<CallInst>(&*I);
      if (CI->getCalledFunction()->getName() == YKTRACE_START) {
        // Skip first argument to start_tracing.
        for (auto Arg = CI->arg_begin() + 1; Arg != CI->arg_end(); Arg++) {
          Vec.push_back(Arg->get());
        }
        break;
      }
    }
  }
  return Vec;
}

// Compile a module in-memory and return a pointer to its function.
extern "C" void *compile_module(string TraceName, Module *M, std::map<StringRef, uint64_t> GlobalMapping) {
  std::call_once(LLVMInitialised, initLLVM, nullptr);

  // FIXME Remember memman or allocated memory pointers so we can free the
  // latter when we're done with the trace.
  auto memman = new MemMan();

  auto MPtr = std::unique_ptr<Module>(M);
  string ErrStr;
  ExecutionEngine *EE =
      EngineBuilder(std::move(MPtr))
          .setMemoryManager(std::unique_ptr<MCJITMemoryManager>(memman))
          .setErrorStr(&ErrStr)
          .create();
  if (EE == nullptr)
    errx(EXIT_FAILURE, "Couldn't compile trace: %s", ErrStr.c_str());

  for (auto GM : GlobalMapping) {
    EE->addGlobalMapping(GM.first, GM.second);
  }

  EE->finalizeObject();
  if (EE->hasError())
    errx(EXIT_FAILURE, "Couldn't compile trace: %s",
         EE->getErrorMessage().c_str());

  return (void *)EE->getFunctionAddress(TraceName);
}

#ifndef NDEBUG
// Left trim (in-place) the character `C` from the string `S`.
void lTrim(string &S, const char C) {
  S.erase(0, std::min(S.find_first_not_of(C), S.size() - 1));
}

// Dumps an LLVM Value to a string and trims leading whitespace.
void dumpValueToString(Value *V, string &S) {
  raw_string_ostream RSO(S);
  V->print(RSO);
  lTrim(S, ' ');
}

// Print a trace's instructions "side-by-side" with the instructions from
// which they were derived in the AOT module.
void printSBS(Module *AOTMod, Module *JITMod, ValueToValueMapTy &RevVMap) {
  //assert(JITMod->size() == 1);
  Function *JITFunc = &*JITMod->begin();

  // Find the longest instruction from the JITMod so that we can align the
  // second column.
  size_t LongestJITLine = 0;
  for (auto &JITBlock : *JITFunc) {
    for (auto &JITInst : JITBlock) {
      string Line;
      dumpValueToString(&JITInst, Line);
      auto Len = Line.length();
      if (Len > LongestJITLine)
        LongestJITLine = Len;
    }
  }

  const string JITHeader = string("Trace");
  string Padding = string(LongestJITLine - JITHeader.length(), ' ');
  errs() << "\n\n--- Begin trace dump for " << JITFunc->getName() << " ---\n";
  errs() << JITHeader << Padding << "  | AOT\n";

  // Keep track of the AOT function we are currently in so that we can print
  // inlined function thresholds in the dumped trace.
  StringRef LastAOTFunc;
  for (auto &JITBlock : *JITFunc) {
    for (auto &JITInst : JITBlock) {
      auto V = RevVMap[&JITInst];
      if (V == nullptr) {
        // The instruction wasn't cloned from the AOTMod, so print it only in
        // the JIT column and carry on.
        std::string Line;
        dumpValueToString((Value *)&JITInst, Line);
        errs() << Line << "\n";
        continue;
      }
      Instruction *AOTInst = (Instruction *)&*V;
      assert(AOTInst != nullptr);
      Function *AOTFunc = AOTInst->getFunction();
      assert(AOTFunc != nullptr);
      StringRef AOTFuncName = AOTFunc->getName();
      if (AOTFuncName != LastAOTFunc) {
        // Print an inlining threshold.
        errs() << "# " << AOTFuncName << "()\n";
        LastAOTFunc = AOTFuncName;
      }
      string JITStr;
      dumpValueToString((Value *)&JITInst, JITStr);
      string Padding = string(LongestJITLine - JITStr.length(), ' ');
      string AOTStr;
      dumpValueToString((Value *)AOTInst, AOTStr);
      errs() << JITStr << Padding << "  |  " << AOTStr << "\n";
    }
  }
  errs() << "--- End trace dump for " << JITFunc->getName() << " ---\n";
}
#endif

// Compile an IRTrace to executable code in memory.
//
// The trace to compile is passed in as two arrays of length Len. Then each
// (FuncName[I], BBs[I]) pair identifies the LLVM block at position `I` in the
// trace.
//
// Returns a pointer to the compiled function.
extern "C" void *__ykllvmwrap_irtrace_compile(char *FuncNames[], size_t BBs[],
                                              size_t Len,
                                              char *FNames[], size_t FAddrs[],
                                              size_t FLen) {
  ThreadSafeModule *ThreadAOTMod = getThreadAOTMod();
  // Getting the module without acquiring the context lock is safe in this
  // instance since ThreadAOTMod is not shared between threads.
  Module *AOTMod = ThreadAOTMod->getModuleUnlocked();
  AOTMod->dump();
  LLVMContext &JITContext = AOTMod->getContext();
  auto JITMod = new Module("", JITContext);
  uint64_t TraceIdx = NextTraceIdx.fetch_add(1);
  if (TraceIdx == numeric_limits<uint64_t>::max())
    errx(EXIT_FAILURE, "trace index counter overflowed");

  // Get var args from start_tracing call.
  auto Inputs = get_trace_inputs(AOTMod->getFunction(FuncNames[0]), BBs[0]);

  std::vector<Type *> InputTypes;
  for (auto Val : Inputs) {
    InputTypes.push_back(Val->getType());
  }

  // Create function to store compiled trace.
  string TraceName = string(TRACE_FUNC_PREFIX) + to_string(TraceIdx);
  llvm::FunctionType *FType =
      llvm::FunctionType::get(Type::getVoidTy(JITContext), InputTypes, false);
  llvm::Function *DstFunc = llvm::Function::Create(
      FType, Function::InternalLinkage, TraceName, JITMod);
  DstFunc->setCallingConv(CallingConv::C);

  // Create entry block and setup builder.
  auto DstBB = BasicBlock::Create(JITContext, "", DstFunc);
  llvm::IRBuilder<> Builder(JITContext);
  Builder.SetInsertPoint(DstBB);

  llvm::ValueToValueMapTy VMap;
#ifndef NDEBUG
  llvm::ValueToValueMapTy RevVMap;
#endif
  // Variables that are used (but not defined) inbetween start and stop tracing
  // need to be replaced with function arguments which the user passes into the
  // compiled trace. This loop creates a mapping from those original variables
  // to the function arguments of the compiled trace function.
  for (size_t Idx = 0; Idx != Inputs.size(); Idx++) {
    Value *OldVal = Inputs[Idx];
    Value *NewVal = DstFunc->getArg(Idx);
    assert(NewVal->getType()->isPointerTy());
    VMap[OldVal] = NewVal;
  }

  // A pointer to the call to YKTRACE_START in the AOT module (once
  // encountered). When this changes from NULL to non-NULL, then we start
  // copying instructions from the AOT module into the JIT module.
  Instruction *StartTracingInstr = nullptr;

  std::vector<CallInst *> inlined_calls;
  CallInst *last_call = nullptr;
  std::vector<GlobalVariable *> cloned_globals;
  std::map<StringRef, uint64_t> globalmappings;
  size_t inline_stack_count = 0;

  // Iterate over the PT trace and stitch together all traced blocks.
  for (size_t Idx = 0; Idx < Len; Idx++) {
    auto FuncName = FuncNames[Idx];

    // Get a traced function so we can extract blocks from it.
    Function *F = AOTMod->getFunction(FuncName);
    if (!F)
      errx(EXIT_FAILURE, "can't find function %s", FuncName);

    // Skip to the correct block.
    auto It = F->begin();
    std::advance(It, BBs[Idx]);
    BasicBlock *BB = &*It;

    // Iterate over all instructions within this block and copy them over
    // to our new module.
    for (auto I = BB->begin(); I != BB->end(); I++) {

      // If we've returned from a call skip ahead to the instruction where we
      // left off.
      if (last_call != nullptr) {
        if (&*I == last_call) {
          last_call = nullptr;
        }
        //errs() << "Skip until last call: ";
        //I->dump();
        continue;
      }

      if (inline_stack_count > 0) {
        if (isa<CallInst>(I)) {
          CallInst *CI = cast<CallInst>(&*I);
          Function *CF = CI->getCalledFunction();
          Function *AOTF = AOTMod->getFunction(CF->getName());
          if (AOTF != nullptr && AOTF->getLinkage() != GlobalValue::ExternalLinkage) {
            inlined_calls.push_back(CI);
            inline_stack_count += 1;
            I->dump();
            errs() << "ISC: " << inline_stack_count << "\n";
          }
        }

        if (isa<ReturnInst>(I)) {
          inline_stack_count -= 1;
          last_call = inlined_calls.back();
          inlined_calls.pop_back();
          I->dump();
          errs() << "ISC: " << inline_stack_count << "\n";
        }
      }

      if (inline_stack_count > 0) {
        continue;
      }

      // Skip calls to debug intrinsics (e.g. @llvm.dbg.value). We don't
      // currently handle debug info and these "pseudo-calls" cause our blocks
      // to be prematurely terminated.
      if (isa<DbgInfoIntrinsic>(I))
        continue;

      if (isa<CallInst>(I)) {
        CallInst *CI = cast<CallInst>(&*I);
        Function *CF = CI->getCalledFunction();
        StringRef CFName = CF->getName();
        if (CF == nullptr)
          continue;

        if (CF->getName() == YKTRACE_START) {
          StartTracingInstr = &*I;
          continue;
        } else if (CF->getName() == YKTRACE_STOP) {
          break;
        } else if (AOTMod->getFunction(CF->getName()) != nullptr) {
          // We have IR for this function.
          bool finline = true;
          for (CallInst *cinst : inlined_calls) {
            // Have we inlined this call already? Then this is recursion.
            if (cinst->getCalledFunction() == CF) {
              inlined_calls.push_back(CI);
              inline_stack_count = 1;
              errs() << "Don't inline: ";
              I->dump();
              // Don't inline this function again and leave the call intact.
              // If we haven't already done so create an external definition
              // for it, but only do so once.
              if (globalmappings.count(CFName) == 0) {
                  auto DeclFunc = llvm::Function::Create(CF->getFunctionType(),
                          GlobalValue::ExternalLinkage, CF->getName(), JITMod);
                  VMap[CF] = DeclFunc;
                  for (size_t i = 0; i < FLen; i++) {
                      char *mapname = FNames[i];
                      uint64_t mapaddr = FAddrs[i];
                      if (strcmp(mapname, CFName.data()) == 0) {
                        globalmappings.insert(std::pair<StringRef, uint64_t>(CFName, mapaddr));
                        break;
                      }
                  }
              }
              finline = false;
              break;
            }
          }
          // Skip remainder of this block and remember where we stopped so we
          // can continue tracing from this position after returning from the
          // inlined call.
          // FIXME Deal with calls we cannot or don't want to inline.
          if (StartTracingInstr != nullptr && finline) {
            errs() << "Rewrite args:";
            I->dump();
            inlined_calls.push_back(CI);
            // During inlining, remap function arguments to the variables
            // passed in by the caller.
            for (unsigned int i = 0; i < CI->arg_size(); i++) {
              Value *Var = CI->getArgOperand(i);
              Value *Arg = CF->getArg(i);
              // If the operand has already been cloned into JITMod then we need
              // to use the cloned value in the VMap.
              if (VMap[Var] != nullptr)
                Var = VMap[Var];
              VMap[Arg] = Var;
            }
            break;
          }
        } else {
          // We don't have IR for this function.
        }
      }

      // We don't start copying instructions into the JIT module until we've
      // seen the call to YKTRACE_START.
      if (StartTracingInstr == nullptr)
        continue;

      if (llvm::isa<llvm::BranchInst>(I)) {
        // FIXME Replace all branch instruction with guards.
        continue;
      }

      if (isa<ReturnInst>(I)) {
        errs() << "Remap return";
        I->dump();
        last_call = inlined_calls.back();
        inlined_calls.pop_back();
        last_call->dump();
        // Replace the return variable of the call with its return value.
        // Since the return value will have already been copied over to the
        // JITModule, make sure we look up the copy.
        auto OldRetVal = ((ReturnInst *)&*I)->getReturnValue();
        if (isa<Constant>(OldRetVal)) {
          errs() << "Constant";
          VMap[last_call] = OldRetVal;
        } else {
          OldRetVal->dump();
          auto NewRetVal = VMap[OldRetVal];
          errs() << "bla";
          NewRetVal->dump();
          errs() << "bla";
          last_call->dump();
          VMap[last_call] = NewRetVal;
        }
        break;
      }

      errs() << "Add: ";
      I->dump();
      // If execution reaches here, then the instruction I is to be copied into
      // JITMod. Before we can do this, we have to scan the instruction's
      // operands checking that each is defined in JITMod.
      for (unsigned OpIdx = 0; OpIdx < I->getNumOperands(); OpIdx++) {
        Value *Op = I->getOperand(OpIdx);
        if (VMap[Op] == nullptr) {
          // The operand is undefined in JITMod.
          Type *OpTy = Op->getType();
          if (isa<llvm::AllocaInst>(Op)) {
            // In the AOT module, the operand is allocated on the stack with an
            // `alloca`, but this variable is as-yet undefined in the JIT
            // module.
            //
            // This happens because LLVM has a tendency to move allocas up to
            // the first block of a function, and if we didn't trace that block
            // (e.g. we started tracing in a later block), then we will have
            // missed those allocations. In these cases we materialise the
            // allocations as we see them used in code that *was* traced.
            Value *Alloca = Builder.CreateAlloca(
                OpTy->getPointerElementType(), OpTy->getPointerAddressSpace());
            VMap[Op] = Alloca;
          } else if (isa<GlobalVariable>(Op)) {
            // If there's a reference to a GlobalVariable, copy it over to the
            // new module.
            GlobalVariable *OldGV = cast<GlobalVariable>(Op);
            if (OldGV->isConstant()) {
              // Global variable is a constant so just copy it into the trace.
              // We don't need to check if this global already exists, since
              // we're skipping any operand that's already been cloned into the
              // VMap.
              GlobalVariable *GV = new GlobalVariable(
                  *JITMod, OldGV->getValueType(), OldGV->isConstant(),
                  OldGV->getLinkage(), (Constant *)nullptr, OldGV->getName(),
                  (GlobalVariable *)nullptr, OldGV->getThreadLocalMode(),
                  OldGV->getType()->getAddressSpace());
              GV->copyAttributesFrom(&*OldGV);
              cloned_globals.push_back(OldGV);
              VMap[OldGV] = GV;
            } else {
              // FIXME Allow trace to write to mutable global variables.
              errx(EXIT_FAILURE, "Non-const global variable %s",
                   OldGV->getName().data());
            }
          } else if (isa<Constant>(Op)) {
            // The operand is a constant, so leave it as is.
            VMap[Op] = Op;
            continue;
          } else if (Op == StartTracingInstr) {
            // The value generated by StartTracingInstr is the thread tracer.
            // At some optimisation levels, this gets stored in an alloca'd
            // stack space. Since we've stripped the instruction that generates
            // that value (from the JIT module), we have to make a dummy stack
            // slot to keep LLVM happy.
            Value *NullVal = Constant::getNullValue(OpTy);
            VMap[Op] = NullVal;
          }
        }
      }

      // Shortly we will copy the instruction into the JIT module. We start by
      // cloning the instruction.
      auto NewInst = &*I->clone();

      // FIXME: For now we strip debugging meta-data from the JIT module just
      // so that the module will verify and compile. In the long run we should
      // include the debug info for the trace code. This would entail copying
      // over the various module-level debugging declarations that are
      // dependencies of instructions with !dbg meta-data attached.
      if (NewInst->hasMetadata()) {
        SmallVector<std::pair<unsigned, MDNode *>> InstrMD;
        NewInst->getAllMetadata(InstrMD);
        for (auto &MD : InstrMD) {
          if (MD.first != LLVMContext::MD_dbg)
            continue;
          NewInst->setMetadata(MD.first, NULL);
        }
      }

      // Since the instruction operands still reference values from the AOT
      // module, we must remap them to point to new values in the JIT module.
      llvm::RemapInstruction(NewInst, VMap, RF_NoModuleLevelChanges);
      VMap[&*I] = NewInst;

#ifndef NDEBUG
      RevVMap[NewInst] = &*I;
#endif

      // And finally insert the new instruction into the JIT module.
      Builder.Insert(NewInst);
    }
  }
  Builder.CreateRetVoid();

  // Fix initialisers/referrers for copied global variables.
  // FIXME Do we also need to copy Linkage, MetaData, Comdat?
  for (GlobalVariable *G : cloned_globals) {
    GlobalVariable *NewGV = cast<GlobalVariable>(VMap[G]);
    if (G->isDeclaration())
      continue;

    if (G->hasInitializer())
      NewGV->setInitializer(MapValue(G->getInitializer(), VMap));
  }

#ifndef NDEBUG
  char *SBS = getenv("YK_PRINT_IR_SBS");
  if ((SBS != nullptr) && (strcmp(SBS, "1") == 0)) {
    printSBS(AOTMod, JITMod, RevVMap);
  }
  llvm::verifyModule(*JITMod, &llvm::errs());
#endif

  auto PrintIR = std::getenv("YK_PRINT_IR");
  if (PrintIR != nullptr) {
    if (strcmp(PrintIR, "1") == 0) {
      // Print out the compiled trace's IR to stderr.
      JITMod->dump();
    }
  }

  // Compile IR trace and return a pointer to its function.
  return compile_module(TraceName, JITMod, globalmappings);
}
