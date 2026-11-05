// The processor the game is built for, checked before anything else runs.
//
// The game is compiled for x86-64-v2 (.cargo/config.toml, MW2_X86_LEVEL in
// CMakeLists.txt), which adds SSSE3, SSE4.1, SSE4.2 and POPCNT to the plain x86-64
// baseline. The recompiled code needs SSSE3 most: the 360's vector unit reads memory
// big-endian, so nearly every vector load, store and permute is a byte shuffle, which
// without pshufb SIMDe makes into sixteen single-byte moves.
//
// A processor without them would stop on the first such instruction, perhaps in a
// static constructor, with nothing said. So this runs ahead of every unprioritised
// constructor. It is compiled for v2 with the rest of this crate, unlike the C++ file
// it replaces, which is why it is nothing but cpuid, bit tests and calls: the compiler
// has no vector code here to use those instructions on, and xtask/src/check_hooks.rs
// disassembles it to be sure.

use crate::runtime::install::spawn;
use std::arch::x86_64::__cpuid;
use std::ffi::{c_int, c_void};

const SSSE3: u32 = 1 << 9;
const SSE4_1: u32 = 1 << 19;
const SSE4_2: u32 = 1 << 20;
const POPCNT: u32 = 1 << 23;

unsafe extern "C" {
    #[cfg(windows)]
    #[link_name = "_write"]
    fn write(fd: c_int, text: *const c_void, size: u32) -> i32;
    #[cfg(not(windows))]
    fn write(fd: c_int, text: *const c_void, size: usize) -> isize;
    fn _Exit(code: c_int) -> !;
}

#[inline(never)]
#[unsafe(no_mangle)]
extern "C" fn mw2_check_processor() {
    let needed = SSSE3 | SSE4_1 | SSE4_2 | POPCNT;
    if __cpuid(1).ecx & needed == needed {
        return;
    }
    too_old();
}

#[cold]
#[inline(never)]
fn too_old() -> ! {
    const TEXT: &std::ffi::CStr = c"This processor is too old for the game: it needs SSE4.2, which Intel Core \
processors have had since 2008 and AMD processors since 2011.\n";
    let bytes = TEXT.to_bytes();
    unsafe { write(2, bytes.as_ptr().cast(), bytes.len() as _) };
    spawn::message_box(TEXT);
    unsafe { _Exit(1) }
}

// constructor(101), as clang spells it for each format, and writable as clang makes
// it: the linker leaves a read-only .ctors out of the table the C runtime goes through,
// and the check would quietly not run on Windows.
#[used]
#[cfg_attr(not(windows), unsafe(link_section = ".init_array.00101"))]
#[cfg_attr(windows, unsafe(link_section = ".ctors.65434"))]
static mut CHECK_PROCESSOR: extern "C" fn() = mw2_check_processor;
