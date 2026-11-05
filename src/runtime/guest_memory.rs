// The recompiled code addresses memory as a 32-bit offset from a single host base
// pointer, so the whole guest address space is one reservation.
//
// Layout (guest addresses):
//   0x82000000 .. 0x83F00000   XEX image            (PPC_IMAGE_BASE + PPC_IMAGE_SIZE)
//   0x83F00000 .. +CODE_SIZE*2 function lookup table (see PPC_LOOKUP_FUNC)
//   0x84600000 .. 0xA0000000   general heap         (NtAllocateVirtualMemory)
//   0xA0000000 .. 0xC0000000   "physical" heap      (MmAllocatePhysicalMemoryEx)
//   0xC0000000 .. 0xE0000000   the same memory again, through the second aperture
//
// The physical range is exactly 512 MB -- the console's whole RAM -- because the title
// turns a virtual address into a physical one by masking off the top three bits, which
// only round-trips if the arena does not run past 0xC0000000.
//
// The console reaches that one bank through several address windows, differing only
// in page size and caching: 0xA0000000 and 0xC0000000 are the same bytes at the same
// offset, and the title uses both -- D3D9 hands the command processor addresses formed
// as (address & 0x1FFFFFFF) - 0x40000000 and then reads its own structures back through
// them. So the second window has to be the same memory here, aliased rather than
// translated on access: the recompiled code dereferences a guest pointer directly, with
// nothing to hook. Here the two windows have to be one piece of memory, or the second
// is a silent 512 MB of zeroes: the reads succeed, return nothing, and the failure
// surfaces somewhere else entirely as a count of zero or a null pointer.
//
// A shared memory object placed over the reservation twice. Nothing else has to know:
// a guest pointer through either window is dereferenced as an offset from the base and
// lands on the same page.

use crate::guest::Memory;
use crate::log::{log_error, log_info};
use ppc_rt::{PPC_CODE_SIZE, PPC_IMAGE_BASE, PPC_IMAGE_SIZE};
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

pub const HEAP_BASE: u32 = 0x84600000;
pub const HEAP_END: u32 = 0xA0000000;
pub const PHYSICAL_BASE: u32 = 0xA0000000;
pub const PHYSICAL_END: u32 = 0xC0000000;
const APERTURE_BASE: u32 = 0xC0000000; // the same memory again
const PHYSICAL_SIZE: usize = (PHYSICAL_END - PHYSICAL_BASE) as usize;
const SPACE: u64 = 0x100000000; // full 32-bit guest space

// runtime/guest_memory.h reads this inline: every guest pointer the C++ follows comes
// through it. Written once, before any other thread starts.
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static mw2_guest_base: AtomicPtr<u8> = AtomicPtr::new(ptr::null_mut());

// The top of the physical heap (kernel/memory.rs), which only rises: what the GPU is handed
// lies below it, and runtime/kernel/physical.h reads it inline to tell its addresses apart.
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static mw2_kernel_physical_top: AtomicU32 = AtomicU32::new(PHYSICAL_BASE);

pub fn base() -> *mut u8 {
    mw2_guest_base.load(Ordering::Relaxed)
}

/// The guest's memory, for what is not called from recompiled code.
#[inline(always)]
pub fn memory() -> Memory {
    unsafe { Memory::new(base()) }
}

#[cfg(not(windows))]
mod system {
    use super::*;
    use crate::log::log_error;
    use std::ffi::{c_int, c_long};

    const PROT_READ_WRITE: c_int = 1 | 2;
    const MAP_SHARED: c_int = 0x01;
    const MAP_PRIVATE: c_int = 0x02;
    const MAP_FIXED: c_int = 0x10;
    const MAP_ANONYMOUS: c_int = 0x20;
    const MAP_NORESERVE: c_int = 0x4000;
    const MAP_FAILED: *mut c_void = usize::MAX as *mut c_void;
    const SYS_MEMFD_CREATE: c_long = 319;

    unsafe extern "C" {
        fn mmap(
            address: *mut c_void,
            size: usize,
            protection: c_int,
            flags: c_int,
            fd: c_int,
            offset: i64,
        ) -> *mut c_void;
        fn syscall(number: c_long, ...) -> c_long;
        fn ftruncate(fd: c_int, size: i64) -> c_int;
        fn close(fd: c_int) -> c_int;
    }

    pub fn reserve_space() -> bool {
        // MAP_NORESERVE: address space only, pages allocated on touch.
        let flags = MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE;
        let space = unsafe { mmap(ptr::null_mut(), SPACE as usize, PROT_READ_WRITE, flags, -1, 0) };
        if space == MAP_FAILED {
            return false;
        }
        mw2_guest_base.store(space.cast(), Ordering::Relaxed);
        true
    }

    pub fn map_physical_aperture() -> bool {
        let fd = unsafe { syscall(SYS_MEMFD_CREATE, c"mw2-physical".as_ptr(), 0 as c_int) } as c_int;
        if fd < 0 {
            log_error!("memfd_create failed");
            return false;
        }
        let mut ok = unsafe { ftruncate(fd, PHYSICAL_SIZE as i64) } == 0;
        for window in [PHYSICAL_BASE, APERTURE_BASE] {
            ok = ok && {
                let at = base().wrapping_add(window as usize).cast();
                let flags = MAP_SHARED | MAP_FIXED | MAP_NORESERVE;
                unsafe { mmap(at, PHYSICAL_SIZE, PROT_READ_WRITE, flags, fd, 0) != MAP_FAILED }
            };
        }
        // The mappings keep the memory alive; the descriptor is not needed.
        unsafe { close(fd) };
        ok
    }

    // The reservation is already mapped read/write; pages are backed on touch.
    pub fn commit(_address: u32, _size: u32) -> bool {
        true
    }

    pub fn commit_on_touch(_address: *const c_void) -> bool {
        false
    }
}

// Windows places a view only over a placeholder of exactly its size (Windows 10 1803
// and later), so the space is reserved as placeholders, split at the two windows, the
// bank mapped into both, and the rest turned into an ordinary reservation.
#[cfg(windows)]
mod system {
    use super::*;
    use crate::runtime::windows::*;
    use std::mem::{size_of, transmute};
    use std::sync::atomic::AtomicUsize;

    type VirtualAlloc2 =
        unsafe extern "system" fn(Handle, *mut c_void, usize, u32, u32, *mut c_void, u32) -> *mut c_void;
    type MapViewOfFile3 =
        unsafe extern "system" fn(Handle, Handle, *mut c_void, u64, usize, u32, u32, *mut c_void, u32) -> *mut c_void;
    static VIRTUAL_ALLOC_2: AtomicUsize = AtomicUsize::new(0);
    static MAP_VIEW_OF_FILE_3: AtomicUsize = AtomicUsize::new(0);

    fn alloc2(at: *mut c_void, size: usize, kind: u32) -> *mut c_void {
        let call = unsafe { transmute::<usize, VirtualAlloc2>(VIRTUAL_ALLOC_2.load(Ordering::Relaxed)) };
        unsafe { call(ptr::null_mut(), at, size, kind, PAGE_NOACCESS, ptr::null_mut(), 0) }
    }

    pub fn reserve_space() -> bool {
        let kernel_base = unsafe { GetModuleHandleW(wide(c"kernelbase.dll").as_ptr()) };
        let alloc = unsafe { GetProcAddress(kernel_base, c"VirtualAlloc2".as_ptr()) };
        let map = unsafe { GetProcAddress(kernel_base, c"MapViewOfFile3".as_ptr()) };
        if alloc.is_null() || map.is_null() {
            log_error!("this version of Windows cannot map memory twice (Windows 10 1803 or later is needed)");
            return false;
        }
        VIRTUAL_ALLOC_2.store(alloc as usize, Ordering::Relaxed);
        MAP_VIEW_OF_FILE_3.store(map as usize, Ordering::Relaxed);
        let space = alloc2(ptr::null_mut(), SPACE as usize, MEM_RESERVE | MEM_RESERVE_PLACEHOLDER);
        mw2_guest_base.store(space.cast(), Ordering::Relaxed);
        !space.is_null()
    }

    pub fn map_physical_aperture() -> bool {
        const _: () = assert!(size_of::<usize>() == 8);
        let high = ((PHYSICAL_SIZE as u64) >> 32) as u32;
        let bank = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                ptr::null_mut(),
                PAGE_READWRITE,
                high,
                PHYSICAL_SIZE as u32,
                ptr::null(),
            )
        };
        if bank.is_null() {
            log_error!("CreateFileMapping failed (%u)", unsafe { GetLastError() });
            return false;
        }
        let map = unsafe { transmute::<usize, MapViewOfFile3>(MAP_VIEW_OF_FILE_3.load(Ordering::Relaxed)) };
        let mut ok = true;
        for window in [PHYSICAL_BASE, APERTURE_BASE] {
            let at = base().wrapping_add(window as usize).cast();
            ok = ok && unsafe { VirtualFree(at, PHYSICAL_SIZE, MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER) } != 0;
            ok = ok && {
                let process = unsafe { GetCurrentProcess() };
                let view = unsafe {
                    map(
                        bank,
                        process,
                        at,
                        0,
                        PHYSICAL_SIZE,
                        MEM_REPLACE_PLACEHOLDER,
                        PAGE_READWRITE,
                        ptr::null_mut(),
                        0,
                    )
                };
                !view.is_null()
            };
        }
        // The views keep the memory alive; the handle is not needed.
        unsafe { CloseHandle(bank) };
        if !ok {
            log_error!("mapping the physical bank failed (%u)", unsafe { GetLastError() });
            return false;
        }
        // What is left of the placeholder, below the bank and above the second window,
        // becomes an ordinary reservation to commit from.
        let aperture_end = APERTURE_BASE as usize + PHYSICAL_SIZE;
        let kind = MEM_RESERVE | MEM_REPLACE_PLACEHOLDER;
        !alloc2(base().cast(), PHYSICAL_BASE as usize, kind).is_null()
            && !alloc2(base().wrapping_add(aperture_end).cast(), SPACE as usize - aperture_end, kind).is_null()
    }

    pub fn commit(address: u32, size: u32) -> bool {
        let at = base().wrapping_add(address as usize).cast();
        !unsafe { VirtualAlloc(at, size as usize, MEM_COMMIT, PAGE_READWRITE) }.is_null()
    }

    pub fn commit_on_touch(address: *const c_void) -> bool {
        let start = base() as usize;
        let at = address as usize;
        if start == 0 || at < start || at >= start + SPACE as usize {
            return false;
        }
        let mut info = MemoryBasicInformation::default();
        if unsafe { VirtualQuery(address, &mut info, size_of::<MemoryBasicInformation>()) } == 0 {
            return false;
        }
        // Another thread touched the same chunk first, and has committed it.
        if info.state == MEM_COMMIT {
            return info.protect & PAGE_READWRITE != 0;
        }
        if info.state != MEM_RESERVE {
            return false;
        }
        const CHUNK: usize = 0x10000;
        !unsafe { VirtualAlloc((at & !(CHUNK - 1)) as *mut c_void, CHUNK, MEM_COMMIT, PAGE_READWRITE) }.is_null()
    }
}

/// Used for stacks and late allocations.
pub fn commit(address: u32, size: u32) -> bool {
    system::commit(address, size)
}

// On Linux the whole space is mapped read/write and a page is backed the first time it
// is touched, wherever it is. Windows only backs what is committed, so the fault
// handler commits a reserved page the guest touches: the same memory, zeroed, on first
// use.
//
// That includes the first pages. A read through a null guest pointer returns zero
// where the console faults, so a title function whose only hazard is a null base needs
// no guard here: G_GetClientScore (sub_82231F10 in the multiplayer) reads
// level.clients[index].score with level.clients null while no game is loaded, which
// iw4x guards on the PC, and returns 0 on both systems.
pub fn commit_on_touch(address: *const c_void) -> bool {
    system::commit_on_touch(address)
}

/// Puts `host` in the table the recompiled code looks a guest function up in
/// (PPC_LOOKUP_FUNC: after the image, a slot per halfword of code), host-endian as
/// the lookup reads it. False when `guest` is outside the code.
pub fn set_function(guest: u32, host: ppc_rt::PPCFunc) -> bool {
    use ppc_rt::PPC_CODE_BASE;
    let offset = guest.wrapping_sub(PPC_CODE_BASE as u32);
    if u64::from(offset) >= PPC_CODE_SIZE {
        return false;
    }
    let slot = PPC_IMAGE_BASE + PPC_IMAGE_SIZE + u64::from(offset) * 2;
    // Committed by initialise(), and written before any guest code runs.
    unsafe { base().add(slot as usize).cast::<Option<ppc_rt::PPCFunc>>().write_unaligned(Some(host)) };
    true
}

/// Reserves the 4 GiB space and commits the regions above.
pub fn initialise() -> *mut u8 {
    if !system::reserve_space() {
        log_error!("could not reserve %llu bytes of address space", SPACE);
        return ptr::null_mut();
    }
    // The bank first: on Windows the rest of the space is only reserved once the views
    // are in place.
    if !system::map_physical_aperture() {
        log_error!("could not alias the physical aperture at %08X", APERTURE_BASE);
        return ptr::null_mut();
    }

    let table_base = (PPC_IMAGE_BASE + PPC_IMAGE_SIZE) as u32;
    let table_size = (PPC_CODE_SIZE * 2) as u32;

    // The physical bank is a mapping, committed whole already.
    if !commit(PPC_IMAGE_BASE as u32, PPC_IMAGE_SIZE as u32)
        || !commit(table_base, table_size)
        || !commit(HEAP_BASE, HEAP_END - HEAP_BASE)
    {
        log_error!("failed to commit guest regions");
        return ptr::null_mut();
    }

    let megabytes = |bytes: u64| bytes as f64 / 1048576.0;
    log_info!("guest space at host %p", base().cast_const().cast::<c_void>());
    log_info!("  image      %08X..%08X  (%.1f MB)", PPC_IMAGE_BASE as u32, table_base, megabytes(PPC_IMAGE_SIZE));
    log_info!(
        "  func table %08X..%08X  (%.1f MB)",
        table_base,
        table_base + table_size,
        megabytes(u64::from(table_size))
    );
    log_info!("  heap       %08X..%08X  (%.1f MB)", HEAP_BASE, HEAP_END, megabytes(u64::from(HEAP_END - HEAP_BASE)));
    log_info!(
        "  physical   %08X..%08X  (%.1f MB)",
        PHYSICAL_BASE,
        PHYSICAL_END,
        megabytes(u64::from(PHYSICAL_END - PHYSICAL_BASE))
    );
    log_info!(
        "  aperture   %08X..%08X  (the same memory, second window)",
        APERTURE_BASE,
        APERTURE_BASE + (PHYSICAL_END - PHYSICAL_BASE)
    );
    base()
}
