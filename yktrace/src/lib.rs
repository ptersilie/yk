//! Utilities for collecting and decoding traces.

mod errors;
use libc::c_void;
use std::ffi::{CStr, CString};
use std::collections::HashMap;
mod hwt;

pub use errors::InvalidTraceError;
pub use hwt::mapper::BlockMap;

/// The different ways by which we can collect a trace.
#[derive(Clone, Copy)]
pub enum TracingKind {
    /// Software tracing.
    SoftwareTracing,
    /// Hardware tracing via a branch tracer (e.g. Intel PT).
    HardwareTracing,
}

impl Default for TracingKind {
    /// Returns the default tracing kind.
    fn default() -> Self {
        // FIXME this should query the hardware for a suitable hardware tracer and failing that
        // fall back on software tracing.
        TracingKind::HardwareTracing
    }
}

/// A globally unique block ID for an LLVM IR block.
#[derive(Debug)]
pub struct IRBlock {
    /// The name of the function containing the block.
    func_name: CString,
    /// The index of the block within the function.
    bb: usize,
}

impl IRBlock {
    pub fn func_name(&self) -> &CStr {
        &self.func_name.as_c_str()
    }

    pub fn bb(&self) -> usize {
        self.bb
    }
}

/// An LLVM IR trace.
pub struct IRTrace {
    blocks: Vec<IRBlock>,
    faddrs: HashMap<CString, u64>,
}

unsafe impl Send for IRTrace {}
unsafe impl Sync for IRTrace {}

impl IRTrace {
    pub(crate) fn new(input: (Vec<IRBlock>, HashMap<CString, u64>)) -> Self {
        Self { blocks: input.0, faddrs: input.1 }
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn get(&self, idx: usize) -> Option<&IRBlock> {
        self.blocks.get(idx)
    }

    pub fn compile(&self) -> *const c_void {
        let len = self.len();
        let mut func_names = Vec::with_capacity(len);
        let mut bbs = Vec::with_capacity(len);
        for blk in &self.blocks {
            func_names.push(blk.func_name().as_ptr());
            bbs.push(blk.bb());
        }

        let mut fnames = Vec::new();
        let mut faddrs = Vec::new();
        for k in self.faddrs.iter() {
            fnames.push(k.0.as_ptr());
            faddrs.push(*k.1);
        }

        unsafe { ykllvmwrap::__ykllvmwrap_irtrace_compile(func_names.as_ptr(), bbs.as_ptr(), len, fnames.as_ptr(), faddrs.as_ptr(), faddrs.len()) }
    }
}

/// Binary executable trace code.
pub struct CompiledTrace {}

unsafe impl Send for CompiledTrace {}
unsafe impl Sync for CompiledTrace {}

/// Represents a thread which is currently tracing.
pub struct ThreadTracer {
    /// The tracing implementation.
    t_impl: Box<dyn ThreadTracerImpl>,
}

impl ThreadTracer {
    /// Stops tracing on the current thread, returning a IR trace on success.
    pub fn stop_tracing(mut self) -> Result<IRTrace, InvalidTraceError> {
        let trace = self.t_impl.stop_tracing();
        if let Ok(inner) = &trace {
            if inner.len() == 0 {
                return Err(InvalidTraceError::EmptyTrace);
            }
        }
        trace
    }
}

// An generic interface which tracing backends must fulfill.
trait ThreadTracerImpl {
    /// Stops tracing on the current thread, returning the IR trace on success.
    fn stop_tracing(&mut self) -> Result<IRTrace, InvalidTraceError>;
}

/// Start tracing on the current thread using the specified tracing kind.
/// Each thread can have at most one active tracer; calling `start_tracing()` on a thread where
/// there is already an active tracer leads to undefined behaviour.
pub fn start_tracing(kind: TracingKind) -> ThreadTracer {
    match kind {
        TracingKind::SoftwareTracing => todo!(),
        TracingKind::HardwareTracing => hwt::start_tracing(),
    }
}
