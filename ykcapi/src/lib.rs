//! This crate exports the Yk API via the C ABI.
//!
//! We use a dedicated crate for exporting to C, as you quickly get into linkage trouble if you try
//! and mix Rust dynamic libraries (namely you can get duplicate copies of dependencies).
//!
//! The sane solution is to have only one `cdylib` crate in our workspace (this crate) and all
//! other crates are regular `rlibs`.

#![feature(bench_black_box)]
#![feature(c_variadic)]
#![feature(naked_functions)]
#![feature(once_cell)]

#[cfg(feature = "yk_testing")]
mod testing;

use std::arch::asm;
use std::convert::{TryFrom, TryInto};
use std::ffi::c_void;
use std::{ptr, slice};
use ykrt::{print_jit_state, HotThreshold, Location, MT};
use yksgi::{self, SGInterp};
use yksmp::{Location as SMLocation, StackMapParser};

/// The first three locations of an LLVM stackmap record, according to the source, are CC, Flags,
/// Num Deopts, which need to be skipped when mapping the stackmap values back to AOT variables.
const SM_REC_HEADER: usize = 3;

#[no_mangle]
pub extern "C" fn yk_mt_new() -> *mut MT {
    let mt = Box::new(MT::new());
    Box::into_raw(mt)
}

#[no_mangle]
pub extern "C" fn yk_mt_drop(mt: *mut MT) {
    unsafe { Box::from_raw(mt) };
}

// The "dummy control point" that is replaced in an LLVM pass.
#[no_mangle]
pub extern "C" fn yk_mt_control_point(_mt: *mut MT, _loc: *mut Location) {
    // Intentionally empty.
}

// The "real" control point, that is called once the interpreter has been patched by ykllvm.
#[no_mangle]
pub extern "C" fn __ykrt_control_point(
    mt: *mut MT,
    loc: *mut Location,
    ctrlp_vars: *mut c_void,
    // Opaque pointer in which to store the result of the control point's caller's return value as
    // interpreted by the stopgap interpreter.
    returnval: *mut c_void,
) -> u64 {
    debug_assert!(!ctrlp_vars.is_null());
    if !loc.is_null() {
        let mt = unsafe { &*mt };
        let loc = unsafe { &*loc };
        return mt.control_point(loc, ctrlp_vars, returnval);
    }
    0
}

#[no_mangle]
pub extern "C" fn yk_mt_hot_threshold_set(mt: &MT, hot_threshold: HotThreshold) {
    mt.set_hot_threshold(hot_threshold);
}

#[no_mangle]
pub extern "C" fn yk_location_new() -> Location {
    Location::new()
}

#[no_mangle]
pub extern "C" fn yk_location_drop(loc: Location) {
    drop(loc)
}

/// Reads out registers spilled to the stack of the previous frame during the deoptimisation
/// routine. The order of the registers are in accordance to the DWARF register number mapping
/// referenced in the SystemV ABI manual (https://uclibc.org/docs/psABI-x86_64.pdf).
struct Registers {
    addr: *const usize,
}

impl Registers {
    /// Creates a Registers struct from a given a pointer on the stack containing spilled
    /// registers.
    fn from_ptr(ptr: *const c_void) -> Registers {
        Registers {
            addr: ptr as *const usize,
        }
    }

    /// Read the spilled register value at the offset `off` from the previous stack frame.
    unsafe fn read_from_stack(&self, off: isize) -> usize {
        ptr::read::<usize>(self.addr.offset(off))
    }

    /// Retrieve the previous frame's register value given by its DWARF register number `id`. This
    /// number additionally functions as an offset into the the spilled stack to find that
    /// register's value.
    #[cfg(target_arch = "x86_64")]
    unsafe fn get(&self, id: u16) -> usize {
        if id > 7 {
            unreachable!(
                "Register #{} currently not saved during deoptimisation.",
                id
            )
        }
        let val = self.read_from_stack(id.try_into().unwrap());
        // Due to the return address being pushed to the stack before we store RSP, its value is
        // off by 8 bytes.
        if id == 7 {
            todo!(); // Check this is still true now that llvm_deoptimize is a naked function.
        }
        val
    }
}

/// Location in terms of basic block index, instruction index, and function name, of a
/// variable in the AOT module. Mirrors the LLVM struct defined in ykllvmwrap/jitmodbuilder.cc.
#[derive(Debug)]
#[repr(C)]
struct AOTVar {
    bbidx: usize,
    instridx: usize,
    fname: *const i8,
    sfidx: usize,
}

/// Address and length of a vector. Mirrors the struct defined in
/// ykllvmwrap/jitmodbuilder.cc.
#[derive(Debug)]
#[repr(C)]
pub struct CVec {
    // FIXME rename to PtrLen
    addr: *const c_void,
    length: usize,
}

/// Reconstructs the stackframes and jumps back to AOT code from where to continue after a guard
/// failure.
#[cfg(target_arch = "x86_64")]
#[naked]
#[no_mangle]
pub extern "C" fn __ykrt_reconstruct_frames(
    newframesptr: *const c_void,
) {
    unsafe {
        asm!(
            // Move size of new stack into rdx.
            "mov rdx, [rdi]",
            // Then adjust the address to where the stack actually starts.
            "add rdi, 8",
            // Make space for the new stack, but use 8 bytes less in order to overwrite this
            // function's return address since we won't be returning there.
            "add rsp, 8",
            "sub rsp, rdx",
            // Copy over the new stack frames.
            "mov rsi, rdi",
            "mov rdi, rsp",
            "call memcpy",
            // Restore callee saved registers.
            "pop r15",
            "pop r14",
            "pop r13",
            "pop r12",
            "pop rbx",
            // Load new return address from stack and jump to it.
            "add rsp, 8",
            "jmp [rsp-8]",
            //"ret",
            options(noreturn)
        )
    }
}

/// Reads the stackmap and saved registers from the given address (i.e. the return address of the
/// deoptimisation call).
#[cfg(target_arch = "x86_64")]
#[no_mangle]
pub extern "C" fn yk_stopgap(
    stackmap: &CVec,
    aotmap: &CVec,
    actframes: &CVec,
    retvalptr: *mut c_void, // this is the bottom frame address now
    retaddr: usize,
    rsp: *const c_void,
) -> u64 {
    // FIXME: remove once we have a stopgap interpreter.
    #[cfg(feature = "yk_jitstate_debug")]
    print_jit_state("enter-stopgap");

    // Parse AOTMap.
    let aotmap = unsafe { slice::from_raw_parts(aotmap.addr as *const AOTVar, aotmap.length) };

    // Parse active frames vector.
    let activeframes = unsafe {
        slice::from_raw_parts(actframes.addr as *const yksgi::FrameInfo, actframes.length)
    };

    // Restore saved registers from the stack.
    let registers = Registers::from_ptr(rsp);

    println!("frame address: {:?}", retvalptr);

    let mut sginterp = unsafe { SGInterp::new(activeframes, retvalptr) };

    // Parse the stackmap of the JIT module.
    println!("stopgap: parse JIT stackmap");
    let slice = unsafe { slice::from_raw_parts(stackmap.addr as *mut u8, stackmap.length) };
    let map = StackMapParser::parse(slice).unwrap();
    let locs = map.get(&retaddr.try_into().unwrap()).unwrap();

    // Extract live values from the stackmap.
    // Skip first 3 locations as they don't relate to any of our live variables.
    for (i, l) in locs.iter().skip(SM_REC_HEADER).enumerate() {
        println!("--");
        match l {
            SMLocation::Register(reg, _size) => {
                let _val = unsafe { registers.get(*reg) };
                todo!();
            }
            SMLocation::Direct(reg, off, _size) => {
                // When using `llvm.experimental.deoptimize` then direct locations should always be
                // in relation to RBP.
                assert_eq!(*reg, 6);
                let addr = unsafe { registers.get(*reg) as *mut u8 };
                let addr = unsafe { addr.offset(isize::try_from(*off).unwrap()) };
                let aot = &aotmap[i];
                println!("direct addr: {:?}", addr);
                unsafe {
                    sginterp.var_init(
                        aot.bbidx,
                        aot.instridx,
                        std::ffi::CStr::from_ptr(aot.fname),
                        aot.sfidx,
                        addr as u64,
                    );
                }
            }
            SMLocation::Indirect(reg, off, size) => {
                let addr = unsafe { registers.get(*reg) as *mut u8 };
                let addr = unsafe { addr.offset(isize::try_from(*off).unwrap()) };
                println!("direct addr: {:?}", addr);
                let v = match *size {
                    1 => unsafe { ptr::read::<u8>(addr as *mut u8) as u64 },
                    2 => unsafe { ptr::read::<u16>(addr as *mut u16) as u64 },
                    4 => unsafe { ptr::read::<u32>(addr as *mut u32) as u64 },
                    8 => unsafe { ptr::read::<u64>(addr as *mut u64) as u64 },
                    _ => unreachable!(),
                };
                let aot = &aotmap[i];
                unsafe {
                    sginterp.var_init(
                        aot.bbidx,
                        aot.instridx,
                        std::ffi::CStr::from_ptr(aot.fname),
                        aot.sfidx,
                        v,
                    );
                }
            }
            SMLocation::Constant(v) => {
                let aot = &aotmap[i];
                println!("const");
                unsafe {
                    sginterp.var_init(
                        aot.bbidx,
                        aot.instridx,
                        std::ffi::CStr::from_ptr(aot.fname),
                        aot.sfidx,
                        *v as u64,
                    );
                }
            }
            SMLocation::LargeConstant(_v) => {
                todo!();
            }
        }
    }
    let ret = sginterp.reconstruct_stackmap(retvalptr);
    println!("RET: {}", ret);
    //let ret = unsafe { sginterp.interpret() };
    print_jit_state("exit-stopgap");
    ret
}

/// The `__llvm__deoptimize()` function required by `llvm.experimental.deoptimize` intrinsic, that
/// we use for exiting to the stop-gap interpreter on guard failure.
#[cfg(target_arch = "x86_64")]
#[naked]
#[no_mangle]
pub extern "C" fn __llvm_deoptimize(
    stackmap: *const c_void,
    aotmap: *const c_void,
    frames: *const c_void,
    retval: *mut c_void,
) -> u64 {
    // Push all registers to the stack before they can be clobbered, so that we can find their
    // values after parsing in the stackmap. The order in which we push the registers is equivalent
    // to the Sys-V x86_64 ABI, which the stackmap format uses as well. This function has the
    // "naked" attribute to keep the optimiser from generating the function prologue which messes
    // with the RSP value of the previous stack frame (this value is often referenced by the
    // stackmap).
    unsafe {
        asm!(
            // Save registers to the stack.
            // FIXME: Add other registers that may be referenced by the stackmap.
            "push rsp",
            "push rbp",
            "push rdi",
            "push rsi",
            "push rbx",
            "push rcx",
            "push rdx",
            "push rax",
            // Now we need to call yk_stopgap. The arguments need to be in RDI, RSI, RDX,
            // RCX, R8, and R9. The first four arguments (stackmap
            // live variable map, frames, and return value pointer) are already where they
            // need to be as we are just forwarding them from the current function's
            // arguments. The remaining arguments (return address and current stack
            // pointer) need to be in R8 and R9. The return address was at [RSP] before
            // the above pushes, so to find it we need to offset 8 bytes per push.
            "mov r8, [rsp+64]",
            "mov r9, rsp",
            "sub rsp, 8", // Alignment
            "call yk_stopgap",
            // memcopy new stack over old stack
            // copy correct vals into registers
            //  idea: push register values onto new stack before its copied over
            //  after copying over, delete mmap, then pop the registers
            // jump
            // instead of jump, push address to top of stack. then ret
            //"jmp rbx",
            "add rsp, 72",
            // FIXME: Don't rely on RBP being pushed. Use frame size retrieved from
            // stackmap instead.
            "mov rsp, rbp",
            "pop rbp",
            "ret",
            options(noreturn)
        )
    }
}

#[cfg(not(target_arch = "x86_64"))]
compile_error!("__llvm_deoptimize() not yet implemented for this platform");
