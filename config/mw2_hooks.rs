// The title's side of the Rust backend, what runtime/mmio_hook.h and the top of
// ppc_recomp_shared.h (xtask/src/generate.rs) are to the C++ one.

unsafe extern "C" {
    fn mw2_xma_register_store(address: u32, value: u32);
    fn MW2ReportBadIndirectCall(address: u32) -> !;
}

#[inline(always)]
pub unsafe fn ppc_mm_store_u8(base: *mut u8, address: u32, value: u8) {
    st8(base, address, value);
}

#[inline(always)]
pub unsafe fn ppc_mm_store_u16(base: *mut u8, address: u32, value: u16) {
    st16(base, address, value);
}

// The console's hardware registers are ordinary memory here, so the store lands
// there, and the XMA decoder's window is also reported to the runtime: its kick,
// lock and clear registers are commands, not state.
#[inline(always)]
pub unsafe fn ppc_mm_store_u32(base: *mut u8, address: u32, value: u32) {
    st32(base, address, value);
    if (address & 0xFFFF0000) == 0x7FEA0000 {
        mw2_xma_register_store(address, value);
    }
}

#[inline(always)]
pub unsafe fn ppc_mm_store_u64(base: *mut u8, address: u32, value: u64) {
    st64(base, address, value);
}

// A guest address outside the code section would index past the table, into
// memory the guest can write. Bounded here, and an empty slot is reported too.
#[inline(always)]
pub unsafe fn ppc_call_indirect(ctx: &mut PPCContext, base: *mut u8, address: u32) {
    if address.wrapping_sub(PPC_CODE_BASE as u32) >= PPC_CODE_SIZE as u32 {
        MW2ReportBadIndirectCall(address);
    }
    match ppc_lookup_func(base, address) {
        Some(function) => function(ctx, base),
        None => MW2ReportBadIndirectCall(address),
    }
}
