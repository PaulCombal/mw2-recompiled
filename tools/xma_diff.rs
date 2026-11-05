// tools/xma_diff.py's Rust half: what src/runtime/kernel/xma.rs needs of the runtime around
// it -- the guest base, the physical heap's top, a physical heap that puts the contexts
// where the recording had them -- and the calls tools/xma_diff.cpp makes of it. The
// generated root makes these the crate's `runtime` and puts xma.rs under its kernel.

pub mod guest_memory {
    use crate::guest::Memory;
    use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

    pub const HEAP_END: u32 = 0xA000_0000;
    pub const PHYSICAL_BASE: u32 = 0xA000_0000;

    // Switched to a side's guest space before anything of that side runs.
    #[allow(non_upper_case_globals)]
    #[unsafe(no_mangle)]
    pub static mw2_guest_base: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());

    // Everything below 0xE0000000 is the physical arena, for both sides.
    #[allow(non_upper_case_globals)]
    #[unsafe(no_mangle)]
    pub static mw2_kernel_physical_top: AtomicU32 = AtomicU32::new(0xE000_0000);

    pub fn memory() -> Memory {
        unsafe { Memory::new(mw2_guest_base.load(Ordering::Relaxed)) }
    }
}

pub mod platform {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;

    pub fn path(text: &CStr) -> std::path::PathBuf {
        std::ffi::OsStr::from_bytes(text.to_bytes()).into()
    }
}

pub mod kernel {
    use std::sync::atomic::{AtomicU32, Ordering};

    static CONTEXTS_AT: AtomicU32 = AtomicU32::new(0);
    static CALLS: AtomicU32 = AtomicU32::new(0);

    /// Where both sides' contexts go: the recording's address.
    #[unsafe(no_mangle)]
    extern "C" fn rig_set_contexts_at(address: u32) {
        CONTEXTS_AT.store(address, Ordering::Relaxed);
    }

    // Each side asks twice: its first block is at physical address 0, which it gives up,
    // and its second is the recording's address.
    pub fn allocate_physical(_size: u32, _alignment: u32) -> u32 {
        if CALLS.fetch_add(1, Ordering::Relaxed) % 2 == 0 { 0xA000_0000 } else { CONTEXTS_AT.load(Ordering::Relaxed) }
    }

    /// The C++'s (runtime/kernel/kernel.h).
    #[unsafe(no_mangle)]
    extern "C" fn mw2_kernel_allocate_physical(size: u32, alignment: u32) -> u32 {
        allocate_physical(size, alignment)
    }

    pub mod video {
        pub fn to_physical(virtual_address: u32) -> u32 {
            (virtual_address & 0x1FFF_FFFF).wrapping_add((virtual_address >> 20).wrapping_add(0x200) & 0x1000)
        }
    }
}

mod calls {
    use crate::runtime::kernel::xma;

    #[unsafe(no_mangle)]
    extern "C" fn rig_rust_initialise() {
        xma::initialise();
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_rust_allocate() -> u32 {
        xma::allocate_context()
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_rust_release(address: u32) {
        xma::release_context(address);
    }

    /// `value` as the hardware holds it.
    #[unsafe(no_mangle)]
    extern "C" fn rig_rust_register_store(address: u32, value: u32) {
        xma::register_store(address, value);
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_rust_report() {
        xma::report();
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_rust_count_error() -> bool {
        xma::count_error()
    }

    /// Whether FFmpeg is decoding beside the port, whose log is not this side's.
    #[unsafe(no_mangle)]
    extern "C" fn rig_rust_shadow() -> bool {
        xma::SHADOW.load(std::sync::atomic::Ordering::Relaxed)
    }
}
