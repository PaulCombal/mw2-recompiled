// What a hook sees of the guest: its registers, its memory and its functions.
//
// This is the unsafe part, so that hooks need none. It rests on what a hook is
// handed: `ctx` and `base` come from the caller in recompiled code, and `base` is
// the start of the guest's 4 GB, which is why a u32 cannot be out of bounds. A
// read of a page the guest never mapped faults here as it does in the C++.
//
// Guest memory is big-endian and other threads and the GPU watch it, so it is only
// reached through these methods, which swap and are volatile. A `longjmp` in the
// guest can skip any frame here: nothing holds a destructor across a call to it.

use std::ffi::{CStr, c_char};
use std::mem::{needs_drop, offset_of};
use std::ptr;
use std::sync::atomic::Ordering::SeqCst;

pub use ppc_rt::{PPCContext, PPCRegister};

// The same layout as the C++ runtime's, which it shares a context with.
const _: () = {
    assert!(size_of::<PPCContext>() == 2688);
    assert!(offset_of!(PPCContext, fpscr) == 324);
    assert!(offset_of!(PPCContext, v127) == 2624);
};

// Every bit pattern is valid for each field, so reading one is safe. Writing a
// field already is. Never a named constant of this type: see "The rustc bug" in
// the fork's docs/rust-backend.md.
pub trait Register {
    fn as_u32(&self) -> u32;
    fn as_i32(&self) -> i32;
    fn as_u64(&self) -> u64;
    fn as_f64(&self) -> f64;
}

impl Register for PPCRegister {
    #[inline(always)]
    fn as_u32(&self) -> u32 {
        unsafe { self.u32 }
    }

    #[inline(always)]
    fn as_i32(&self) -> i32 {
        unsafe { self.s32 }
    }

    #[inline(always)]
    fn as_u64(&self) -> u64 {
        unsafe { self.u64 }
    }

    #[inline(always)]
    fn as_f64(&self) -> f64 {
        unsafe { self.f64 }
    }
}

#[derive(Copy, Clone)]
pub struct Memory {
    base: *mut u8,
}

impl Memory {
    /// # Safety
    /// `base` is the one recompiled code passes.
    #[inline(always)]
    pub unsafe fn new(base: *mut u8) -> Self {
        // PPC_FUNC_PROLOGUE tells the compiler this, in the C++.
        debug_assert!(base as usize & 0x1F == 0);
        Self { base }
    }

    #[inline(always)]
    pub fn u8_at(self, address: u32) -> u8 {
        unsafe { ppc_rt::ld8(self.base, address) }
    }

    #[inline(always)]
    pub fn u16_at(self, address: u32) -> u16 {
        unsafe { ppc_rt::ld16(self.base, address) }
    }

    #[inline(always)]
    pub fn u32_at(self, address: u32) -> u32 {
        unsafe { ppc_rt::ld32(self.base, address) }
    }

    #[inline(always)]
    pub fn u64_at(self, address: u32) -> u64 {
        unsafe { ppc_rt::ld64(self.base, address) }
    }

    #[inline(always)]
    pub fn store_u8(self, address: u32, value: u8) {
        unsafe { ppc_rt::st8(self.base, address, value) }
    }

    #[inline(always)]
    pub fn store_u16(self, address: u32, value: u16) {
        unsafe { ppc_rt::st16(self.base, address, value) }
    }

    #[inline(always)]
    pub fn store_u32(self, address: u32, value: u32) {
        unsafe { ppc_rt::st32(self.base, address, value) }
    }

    #[inline(always)]
    pub fn store_u64(self, address: u32, value: u64) {
        unsafe { ppc_rt::st64(self.base, address, value) }
    }

    /// Replaces the aligned word at `address` with `desired` if it holds `expected`,
    /// atomically, as the guest's lwarx and stwcx. would.
    #[inline(always)]
    pub fn compare_exchange_u32(self, address: u32, expected: u32, desired: u32) -> bool {
        debug_assert!(address & 3 == 0);
        unsafe { ppc_rt::stwcx(self.base, address, expected.swap_bytes(), desired.swap_bytes()) }
    }

    /// Adds to the aligned word at `address` atomically and returns what it now holds,
    /// as InterlockedIncrement does.
    #[inline(always)]
    pub fn add_i32(self, address: u32, delta: i32) -> i32 {
        debug_assert!(address & 3 == 0);
        let mut old = unsafe { ppc_rt::lwarx(self.base, address) };
        loop {
            let value = (old.swap_bytes() as i32).wrapping_add(delta);
            match unsafe { std::sync::atomic::AtomicU32::from_ptr(self.base.wrapping_add(address as usize).cast()) }
                .compare_exchange_weak(old, (value as u32).swap_bytes(), SeqCst, SeqCst)
            {
                Ok(_) => return value,
                Err(now) => old = now,
            }
        }
    }

    /// Stores to the aligned word at `address` as an atomic store does.
    #[inline(always)]
    pub fn store_atomic_u32(self, address: u32, value: u32) {
        debug_assert!(address & 3 == 0);
        unsafe { std::sync::atomic::AtomicU32::from_ptr(self.base.wrapping_add(address as usize).cast()) }
            .store(value.swap_bytes(), SeqCst)
    }

    /// The recompiled function that starts at `address`, unless the address is outside
    /// the image's code or none starts there (PPC_LOOKUP_FUNC).
    pub fn function_at(self, address: u32) -> Option<GuestCall> {
        if !(ppc_rt::PPC_CODE_BASE..ppc_rt::PPC_CODE_BASE + ppc_rt::PPC_CODE_SIZE).contains(&u64::from(address)) {
            return None;
        }
        unsafe { ppc_rt::ppc_lookup_func(self.base, address) }.map(GuestCall)
    }

    /// memcpy from the host.
    pub fn store_bytes(self, address: u32, bytes: &[u8]) {
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), self.base.wrapping_add(address as usize), bytes.len()) }
    }

    /// memcpy to the host.
    pub fn load_bytes(self, address: u32, bytes: &mut [u8]) {
        unsafe { ptr::copy_nonoverlapping(self.base.wrapping_add(address as usize), bytes.as_mut_ptr(), bytes.len()) }
    }

    /// The `size` bytes at `address`.
    pub fn bytes(self, address: u32, size: usize) -> Vec<u8> {
        let mut bytes = vec![0; size];
        self.load_bytes(address, &mut bytes);
        bytes
    }

    /// The string at `address`, without its zero, reading at most `limit` bytes.
    pub fn string(self, address: u32, limit: usize) -> Vec<u8> {
        (0u32..).take(limit).map(|i| self.u8_at(address.wrapping_add(i))).take_while(|&byte| byte != 0).collect()
    }

    /// memmove: the two may overlap.
    pub fn copy(self, to: u32, from: u32, bytes: u32) {
        unsafe {
            ptr::copy(self.base.wrapping_add(from as usize), self.base.wrapping_add(to as usize), bytes as usize);
        }
    }

    /// memset to zero.
    pub fn zero(self, address: u32, bytes: u32) {
        unsafe { ptr::write_bytes(self.base.wrapping_add(address as usize), 0, bytes as usize) }
    }

    /// The guest pointer at `address`, unless it is null.
    #[inline(always)]
    pub fn pointer_at(self, address: u32) -> Option<u32> {
        Some(self.u32_at(address)).filter(|&pointer| pointer != 0)
    }

    /// Whether the string at `address` is `text`. Stops at the first difference,
    /// as strcmp does, so it reads no further than strcmp would.
    pub fn holds(self, address: u32, text: &CStr) -> bool {
        self.compare(address, text, |guest, wanted| guest == wanted)
    }

    /// The same as I_stricmp does it: only ASCII has a case.
    pub fn holds_any_case(self, address: u32, text: &CStr) -> bool {
        self.compare(address, text, |guest, wanted| guest.eq_ignore_ascii_case(&wanted))
    }

    /// Whether the string at `address` begins with `text`, as strncmp does: it
    /// stops at the first difference.
    pub fn starts_with(self, address: u32, text: &CStr) -> bool {
        (0u32..).zip(text.to_bytes()).all(|(i, &byte)| self.u8_at(address.wrapping_add(i)) == byte)
    }

    fn compare(self, address: u32, text: &CStr, same: impl Fn(u8, u8) -> bool) -> bool {
        let mut at = address;
        for &byte in text.to_bytes_with_nul() {
            if !same(self.u8_at(at), byte) {
                return false;
            }
            at = at.wrapping_add(1);
        }
        true
    }

    /// The string at `address`, for a log line.
    #[inline(always)]
    pub fn text(self, address: u32) -> GuestText {
        GuestText(self.base.wrapping_add(address as usize).cast())
    }
}

// Read when the line is printed, not when this is made: a %s of the guest's bytes
// as they are, which need not be UTF-8.
#[derive(Copy, Clone)]
pub struct GuestText(pub(crate) *const c_char);

pub type GuestCode = unsafe extern "C" fn(&mut PPCContext, *mut u8);

// A function of the table in runtime/title.h.
#[derive(Copy, Clone)]
pub struct GuestFunction {
    current: GuestCode,
    original: GuestCode,
}

impl GuestFunction {
    pub const fn new(current: GuestCode, original: GuestCode) -> Self {
        Self { current, original }
    }

    /// As a guest caller would, through a hook if it has one (GUEST_FUNC).
    #[inline(always)]
    pub fn call(self, ctx: &mut PPCContext, memory: Memory) {
        unsafe { (self.current)(ctx, memory.base) }
    }

    /// The recompiled function itself, for its own hook (GUEST_ORIG).
    #[inline(always)]
    pub fn original(self, ctx: &mut PPCContext, memory: Memory) {
        unsafe { (self.original)(ctx, memory.base) }
    }
}

/// A recompiled function found by its guest address.
#[derive(Copy, Clone)]
pub struct GuestCall(GuestCode);

impl GuestCall {
    pub fn call(self, ctx: &mut PPCContext, memory: Memory) {
        unsafe { (self.0)(ctx, memory.base) }
    }

    /// What C++ calls it as.
    pub fn code(self) -> GuestCode {
        self.0
    }
}

/// A context with every register zero, for a guest thread or call of the runtime's own.
pub fn blank_context() -> PPCContext {
    // Plain registers, for which zero is a value.
    unsafe { std::mem::zeroed() }
}

/// The host's float mode (MXCSR) as it stands, and `ctx`'s idea of it made that.
pub fn load_float_mode(ctx: &mut PPCContext) -> u32 {
    unsafe { ctx.fpscr.loadFromHost() };
    ctx.fpscr.csr
}

/// The host's float mode as it stands.
pub fn host_float_mode() -> u32 {
    #[cfg(target_arch = "x86_64")]
    #[allow(deprecated)]
    unsafe {
        std::arch::x86_64::_mm_getcsr()
    }
    // FPCR, which ppc_rt's FPSCR keeps in MXCSR's place on arm64.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        let fpcr: u64;
        std::arch::asm!("mrs {}, fpcr", out(reg) fpcr, options(nostack, preserves_flags, nomem));
        fpcr as u32
    }
}

/// Puts the host's float mode back to what `host_float_mode` gave.
pub fn set_host_float_mode(csr: u32) {
    // Deprecated in favour of inline assembly, which is what it is.
    #[cfg(target_arch = "x86_64")]
    #[allow(deprecated)]
    unsafe {
        std::arch::x86_64::_mm_setcsr(csr)
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        std::arch::asm!("msr fpcr, {}", in(reg) csr as u64, options(nostack, preserves_flags, nomem));
    }
}

/// Runs `body` and puts every register back as it was. A longjmp out of `body`
/// loses the copy, which is plain data.
pub fn preserving<R>(ctx: &mut PPCContext, body: impl FnOnce(&mut PPCContext) -> R) -> R {
    const { assert!(!needs_drop::<PPCContext>()) };
    let saved = unsafe { ptr::read(ctx) };
    let result = body(ctx);
    unsafe { ptr::write(ctx, saved) };
    result
}

/// Replaces a function of the table: `guest_hook!(T_x, |ctx, memory| { ... })`, the
/// counterpart of GUEST_HOOK. The body is safe code in a function of its own, so
/// that the module's deny(unsafe_code) holds for it and only for the entry point
/// is it lifted.
macro_rules! guest_hook {
    ($name:ident, |$ctx:ident, $memory:ident| $body:block) => {
        const _: () = {
            #[inline(always)]
            fn body($ctx: &mut $crate::guest::PPCContext, $memory: $crate::guest::Memory) $body

            #[allow(unsafe_code)]
            #[unsafe(export_name = $crate::title::hook_symbol!($name))]
            unsafe extern "C" fn hook(ctx: &mut $crate::guest::PPCContext, base: *mut u8) {
                // Every guest function hands back its caller's stack.
                #[cfg(debug_assertions)]
                let stack = $crate::guest::Register::as_u64(&ctx.r1);
                body(ctx, unsafe { $crate::guest::Memory::new(base) });
                #[cfg(debug_assertions)]
                debug_assert_eq!(stack, $crate::guest::Register::as_u64(&ctx.r1), stringify!($name));
            }
        };
    };
}
pub(crate) use guest_hook;

/// Defines a kernel import the title calls: `kernel_import!(NtSetEvent, |ctx, memory| { ... })`
/// is the C++'s PPC_FUNC(__imp__NtSetEvent), and safe code as a hook's body is.
macro_rules! kernel_import {
    ($name:ident, |$ctx:ident, $memory:ident| $body:block) => {
        const _: () = {
            #[inline(always)]
            fn body($ctx: &mut $crate::guest::PPCContext, $memory: $crate::guest::Memory) $body

            #[allow(unsafe_code)]
            #[unsafe(export_name = concat!("__imp__", stringify!($name)))]
            unsafe extern "C" fn import(ctx: &mut $crate::guest::PPCContext, base: *mut u8) {
                body(ctx, unsafe { $crate::guest::Memory::new(base) });
            }
        };
    };
}
pub(crate) use kernel_import;
