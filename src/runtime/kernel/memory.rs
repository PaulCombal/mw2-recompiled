// Guest memory management: the two heaps, and the Nt*VirtualMemory and Mm* imports
// over them.

use super::{STATUS_INVALID_PARAMETER, STATUS_NO_MEMORY, STATUS_SUCCESS};
use crate::guest::{Memory, Register, kernel_import};
use crate::log::log_warn;
use crate::runtime::guest_memory::{self, HEAP_BASE, HEAP_END, PHYSICAL_BASE, PHYSICAL_END, mw2_kernel_physical_top};
use std::collections::BTreeMap;
use std::ffi::CStr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, PoisonError};

// A range of guest address space. Freed blocks are kept, merged with their neighbours
// and handed out again first-fit, and zeroed when they are, as the console's pages come;
// the rest is carved off the top, which only rises, so what lies above it has never been
// used and is zero.
pub struct Heap {
    name: &'static CStr,
    end: u32,
    top: &'static AtomicU32,
    blocks: Mutex<Blocks>,
}

struct Blocks {
    live: BTreeMap<u32, u32>, // address -> size
    free: BTreeMap<u32, u32>,
    allocations: u64,
    reused: u64,
    live_bytes: u64,
}

static VIRTUAL_TOP: AtomicU32 = AtomicU32::new(HEAP_BASE);

pub static VIRTUAL: Heap = Heap::new(c"virtual", HEAP_END, &VIRTUAL_TOP);
pub static PHYSICAL: Heap = Heap::new(c"physical", PHYSICAL_END, &mw2_kernel_physical_top);

/// MW2_REPLAY_GPU (gpu_replay.rs): the physical heap's top as the capture found it, so
/// that the command processor tells the addresses it is handed apart as it did then.
#[cfg(feature = "diagnostics")]
pub fn set_physical_top(top: u32) {
    mw2_kernel_physical_top.store(top, Ordering::Release);
}

impl Heap {
    /// Empty again, from `HEAP_BASE`: tools/hook_diff.py runs each side of a case on
    /// the same heap, so that both are given the same addresses.
    #[cfg(mw2_hook_rig)]
    pub fn reset(&self) {
        let mut blocks = self.blocks();
        blocks.live.clear();
        blocks.free.clear();
        blocks.live_bytes = 0;
        self.top.store(HEAP_BASE, Ordering::Release);
    }

    const fn new(name: &'static CStr, end: u32, top: &'static AtomicU32) -> Self {
        Self {
            name,
            end,
            top,
            blocks: Mutex::new(Blocks {
                live: BTreeMap::new(),
                free: BTreeMap::new(),
                allocations: 0,
                reused: 0,
                live_bytes: 0,
            }),
        }
    }

    fn blocks(&self) -> std::sync::MutexGuard<'_, Blocks> {
        self.blocks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Zero when there is no room. `align` is a power of two.
    pub fn allocate(&self, memory: Memory, size: u32, align: u32) -> u32 {
        if size == 0 {
            return 0;
        }
        let mut blocks = self.blocks();
        blocks.allocations += 1;
        let up = |address: u32| address.wrapping_add(align.wrapping_sub(1)) & !align.wrapping_sub(1);
        let fit = blocks
            .free
            .iter()
            .map(|(&start, &length)| (start, length, up(start)))
            .find(|&(start, length, at)| u64::from(at) + u64::from(size) <= u64::from(start) + u64::from(length));
        if let Some((start, length, at)) = fit {
            blocks.free.remove(&start);
            if at > start {
                blocks.free.insert(start, at - start);
            }
            let after = at.wrapping_add(size);
            if after < start.wrapping_add(length) {
                blocks.free.insert(after, start.wrapping_add(length).wrapping_sub(after));
            }
            memory.zero(at, size);
            blocks.reused += 1;
            return blocks.took(at, size);
        }
        let from = self.top.load(Ordering::Relaxed);
        let at = up(from);
        if u64::from(at) + u64::from(size) > u64::from(self.end) {
            return 0;
        }
        if at > from {
            blocks.free.insert(from, at - from);
        }
        self.top.store(at + size, Ordering::Release);
        blocks.took(at, size)
    }

    /// False when `address` is not the start of a live allocation.
    pub fn free(&self, address: u32) -> bool {
        let mut blocks = self.blocks();
        let Some(mut length) = blocks.live.remove(&address) else { return false };
        let mut start = address;
        blocks.live_bytes -= u64::from(length);
        if let Some(after) = blocks.free.remove(&start.wrapping_add(length)) {
            length = length.wrapping_add(after);
        }
        if let Some((&before, &before_length)) = blocks.free.range(..start).next_back()
            && before.wrapping_add(before_length) == start
        {
            start = before;
            length = length.wrapping_add(before_length);
            blocks.free.remove(&before);
        }
        blocks.free.insert(start, length);
        true
    }

    pub fn high_water(&self) -> u32 {
        self.top.load(Ordering::Acquire)
    }

    pub fn report(&self) {
        let blocks = self.blocks();
        let free_bytes: u64 = blocks.free.values().map(|&size| u64::from(size)).sum();
        crate::log::log_info!(
            "memory: %s heap %llu MB in use, %llu MB free below the high water of %08X; %llu allocations, %llu of them into freed space",
            self.name,
            blocks.live_bytes >> 20,
            free_bytes >> 20,
            self.high_water(),
            blocks.allocations,
            blocks.reused
        );
    }
}

impl Blocks {
    fn took(&mut self, at: u32, size: u32) -> u32 {
        self.live.insert(at, size);
        self.live_bytes += u64::from(size);
        at
    }
}

const MEM_COMMIT: u32 = 0x0000_1000;
const MEM_RESERVE: u32 = 0x0000_2000;
const MEM_RELEASE: u32 = 0x0000_8000;

kernel_import!(NtAllocateVirtualMemory, |ctx, memory| {
    let base_pointer = ctx.r3.as_u32();
    let size_pointer = ctx.r4.as_u32();
    let kind = ctx.r5.as_u32();
    if base_pointer == 0 || size_pointer == 0 {
        ctx.r3.u64 = u64::from(STATUS_INVALID_PARAMETER);
        return;
    }
    let wanted = memory.u32_at(base_pointer);
    let size = memory.u32_at(size_pointer).wrapping_add(0xFFF) & !0xFFF;
    if size == 0 {
        ctx.r3.u64 = u64::from(STATUS_INVALID_PARAMETER);
        return;
    }
    // A commit onto an existing reservation just succeeds in place.
    if wanted != 0 && kind & MEM_COMMIT != 0 && kind & MEM_RESERVE == 0 {
        memory.store_u32(size_pointer, size);
        ctx.r3.u64 = u64::from(STATUS_SUCCESS);
        return;
    }
    let address = VIRTUAL.allocate(memory, size, 0x1000);
    if address == 0 {
        log_warn!("NtAllocateVirtualMemory: out of guest heap (%u bytes)", size);
        ctx.r3.u64 = u64::from(STATUS_NO_MEMORY);
        return;
    }
    memory.store_u32(base_pointer, address);
    memory.store_u32(size_pointer, size);
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

kernel_import!(NtFreeVirtualMemory, |ctx, memory| {
    // (BaseAddress*, RegionSize*, FreeType, DebugMemory). A decommit keeps the
    // reservation, which here is the allocation itself.
    let base_pointer = ctx.r3.as_u32();
    if base_pointer == 0 {
        ctx.r3.u64 = u64::from(STATUS_INVALID_PARAMETER);
        return;
    }
    if ctx.r5.as_u32() & MEM_RELEASE == 0 {
        ctx.r3.u64 = u64::from(STATUS_SUCCESS);
        return;
    }
    let freed = VIRTUAL.free(memory.u32_at(base_pointer));
    ctx.r3.u64 = u64::from(if freed { STATUS_SUCCESS } else { STATUS_INVALID_PARAMETER });
});

kernel_import!(NtQueryVirtualMemory, |ctx, memory| {
    // X_MEMORY_BASIC_INFORMATION; report the whole heap as committed and RW.
    let info = ctx.r4.as_u32();
    if info != 0 {
        let fields = [
            HEAP_BASE,            // BaseAddress
            HEAP_BASE,            // AllocationBase
            0x04,                 // AllocationProtect = PAGE_READWRITE
            HEAP_END - HEAP_BASE, // RegionSize
            0x1000,               // State = MEM_COMMIT
            0x04,                 // Protect
            0x20000,              // Type = MEM_PRIVATE
        ];
        for (at, field) in (info..).step_by(4).zip(fields) {
            memory.store_u32(at, field);
        }
    }
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

kernel_import!(MmAllocatePhysicalMemoryEx, |ctx, memory| {
    let size = ctx.r4.as_u32();
    let lowest = ctx.r6.as_u32();
    let highest = ctx.r7.as_u32();
    // The console hands physical memory out in 64 KB pages (16 MB for the largest
    // requests), so a block is never at an odd 4 KB offset whatever alignment the caller
    // asked for -- and the title relies on it. MW2 takes one 444 MB block for nearly all
    // of its memory with alignment 0x1000, and on a 4 KB boundary its own sub-allocators
    // corrupt the script compiler's data during level loads (docs/runtime.md).
    const PAGE_ALIGNMENT: u32 = 0x10000;
    let alignment = if ctx.r8.as_u32() != 0 { ctx.r8.as_u32() } else { 0x1000 }.max(PAGE_ALIGNMENT);
    let address = PHYSICAL.allocate(memory, size.wrapping_add(0xFFF) & !0xFFF, alignment);
    if address == 0 {
        log_warn!(
            "MmAllocatePhysicalMemoryEx: out of physical heap (%u bytes, align %u, range %08X..%08X; %u MB already handed out)",
            size,
            alignment,
            lowest,
            highest,
            (PHYSICAL.high_water() - PHYSICAL_BASE) / (1024 * 1024)
        );
    }
    ctx.r3.u64 = u64::from(address);
});

kernel_import!(MmFreePhysicalMemory, |ctx, _memory| {
    // (Type, BaseAddress)
    PHYSICAL.free(ctx.r4.as_u32());
    ctx.r3.u64 = 0;
});

// MmGetPhysicalAddress is in video.cpp, next to the Vd* calls that consume it.
kernel_import!(MmQueryAddressProtect, |ctx, _memory| {
    ctx.r3.u64 = 0x04;
});

kernel_import!(MmSetAddressProtect, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

kernel_import!(MmQueryStatistics, |ctx, memory| {
    let statistics = ctx.r3.as_u32();
    if statistics != 0 {
        let total = HEAP_END - HEAP_BASE;
        memory.store_u32(statistics.wrapping_add(4), total); // TotalPhysicalPages-ish; the title only sanity-checks these
        memory.store_u32(statistics.wrapping_add(8), total / 2);
    }
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

/// Guest memory from the virtual heap, zeroed; zero when there is none.
pub fn allocate_guest(size: u32, alignment: u32) -> u32 {
    VIRTUAL.allocate(guest_memory::memory(), size, alignment)
}
