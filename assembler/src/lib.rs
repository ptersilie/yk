mod backend;

use libc;
use std::mem;
use std::process::Command;

// getpagesize() (sycall)
const PAGE_SIZE: usize = 4096;

struct Assembler {
    memmap: *mut u8,
    instrptr: usize
}

impl Assembler {

    pub fn new() -> Self {
        // Create a single page of memory to store our instructions in and make it writable
        let memmap = unsafe {
            let mut page: *mut libc::c_void = mem::MaybeUninit::uninit().as_mut_ptr();
            libc::posix_memalign(&mut page, PAGE_SIZE, PAGE_SIZE);
            libc::mprotect(page, PAGE_SIZE, libc::PROT_WRITE);
            // Transmute to raw u8 pointer
            let memmap: *mut u8 = mem::transmute(page);
            // TODO correct use of MaybeUninit::assume_init()
            memmap
        };

        Assembler {
            memmap,
            instrptr: 0
        }
    }

    pub fn add_instruction(&mut self, instr: &str) {
        let mc = backend::encode(backend::EncoderBackend::RasmBackend, instr);
        unsafe {
            let off = self.memmap.offset(self.instrptr as isize);
            libc::memcpy(off as *mut _, mc.as_ptr() as *const _, mc.len());
        }
        self.instrptr += mc.len();
    }

    fn make_executable(&self) {
        unsafe {
            libc::mprotect(self.memmap as *mut _, PAGE_SIZE, libc::PROT_EXEC);
        }
    }

    fn function_pointer(&self) -> fn() -> i64 {
        unsafe {
            mem::transmute(self.memmap)
        }
    }
}

#[cfg(test)]
mod tests {

    use super::Assembler;

    #[test]
    pub(crate) fn test_basic() {
        let mut assemb = Assembler::new();
        assemb.add_instruction("mov rdx, 3");
        assemb.add_instruction("mov rcx, 4");
        assemb.add_instruction("add rcx, rdx");
        assemb.add_instruction("mov rax, rcx");
        assemb.add_instruction("ret");
        assemb.make_executable();
        let func = assemb.function_pointer();
        assert_eq!(func(), 7);
    }
}
