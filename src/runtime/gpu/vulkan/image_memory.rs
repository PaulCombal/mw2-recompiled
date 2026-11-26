// Memory for the texture cache's images, carved out of large blocks.
//
// A level streams tens of thousands of textures through the cache, most of them a few
// kilobytes. An allocation of its own for each is what a driver is worst at: freeing one
// took over half a millisecond with forty thousand live, and some drivers allow only 4096
// at all. A block is one allocation of BLOCK_BYTES of one memory type, and a texture gets
// a range of it, so releasing a texture is bookkeeping and never a call into the driver.
// What does not fit a quarter of a block gets an allocation of its own.
//
// Making a block is the driver's slowest call here, about 10 ms on a Renoir iGPU, and it
// came at a draw, where it cost a frame. So one block is always ready, of the type the
// last was made of: made ahead on a thread of its own as soon as the one before is taken,
// or a block emptied and kept instead of given back. One block more than is in use, and
// one made at a draw only when two are wanted before the thread has made the second.
//
// One thread: the ring consumer, which owns the texture cache that owns this; the block
// made ahead is the helper thread's until the consumer takes it.
#![deny(unsafe_op_in_unsafe_fn)]

use super::device::Vk;
use ash::vk;
use std::collections::BTreeMap;
use std::thread::JoinHandle;

pub const BLOCK_BYTES: u64 = 64 << 20;

/// A range of a block, or memory of its own (`block` u32::MAX).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Range {
    pub memory: vk::DeviceMemory,
    pub offset: u64,
    pub size: u64,
    pub block: u32,
}

impl Default for Range {
    fn default() -> Self {
        Range { memory: vk::DeviceMemory::null(), offset: 0, size: 0, block: u32::MAX }
    }
}

impl Range {
    #[inline]
    pub fn is_some(&self) -> bool {
        self.memory != vk::DeviceMemory::null()
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Totals {
    pub blocks: u64,
    // Blocks made and given back over the run, the time the device took to make them and
    // the longest it took for one, and the allocations of their own.
    pub made: u64,
    pub given_back: u64,
    pub making_nanos: u64,
    pub longest_nanos: u64,
    pub alone_made: u64,
    // Blocks made ahead, taken from there (and of those, waited for), and empty blocks
    // kept as the next instead of given back.
    pub made_ahead: u64,
    pub taken_ahead: u64,
    pub waited_ahead: u64,
    pub kept: u64,
    // Blocks made at a draw, on the consumer, and the time they took there.
    pub made_now: u64,
    pub made_now_nanos: u64,
    pub block_bytes: u64,
    pub used_bytes: u64,
    pub alone: u64,
    pub alone_bytes: u64,
}

#[derive(Default)]
struct Block {
    memory: vk::DeviceMemory,
    ty: u32,
    used: u64,
    // The free ranges by offset, neighbours merged, so a block nothing is left on is one
    // range again.
    free: BTreeMap<u64, u64>,
}

impl Block {
    /// The first free range that holds `size` at `alignment`: its offset, carved out.
    fn carve(&mut self, size: u64, alignment: u64) -> Option<u64> {
        let mut found = None;
        for (&start, &length) in &self.free {
            let end = start.wrapping_add(length);
            let at = start.wrapping_add(alignment).wrapping_sub(1) / alignment * alignment;
            if at.wrapping_add(size) > end {
                continue;
            }
            found = Some((start, end, at));
            break;
        }
        let (start, end, at) = found?;
        self.free.remove(&start);
        if at > start {
            self.free.insert(start, at - start);
        }
        if at.wrapping_add(size) < end {
            self.free.insert(at.wrapping_add(size), end.wrapping_sub(at.wrapping_add(size)));
        }
        self.used = self.used.wrapping_add(size);
        Some(at)
    }
}

/// The next block: being made on the helper thread, or made (or kept) and waiting.
enum Next {
    Making(u32, JoinHandle<(vk::DeviceMemory, u64)>),
    Made(u32, vk::DeviceMemory),
}

#[derive(Default)]
pub struct ImageMemory {
    blocks: Vec<Block>, // a freed block's entry is kept, empty, and reused
    last: u32,          // the block the last range came from
    totals: Totals,
    next: Option<Next>,
}

impl ImageMemory {
    /// A range that meets `needs` in memory of type `ty`; empty when the device has none
    /// left.
    ///
    /// # Safety
    /// `vk` is the device the ranges are of, the same for every call until shutdown.
    pub unsafe fn allocate(&mut self, vk: &Vk, needs: &vk::MemoryRequirements, ty: u32) -> Range {
        let mut range = Range { size: needs.size, ..Range::default() };
        let alignment = if needs.alignment != 0 { needs.alignment } else { 1 };
        if needs.size > BLOCK_BYTES / 4 {
            let allocate =
                vk::MemoryAllocateInfo { allocation_size: needs.size, memory_type_index: ty, ..Default::default() };
            // SAFETY: the caller's device.
            if unsafe { vk.allocate_memory(&allocate, &mut range.memory) } != vk::Result::SUCCESS {
                return Range::default();
            }
            self.totals.alone_made = self.totals.alone_made.wrapping_add(1);
            self.totals.alone = self.totals.alone.wrapping_add(1);
            self.totals.alone_bytes = self.totals.alone_bytes.wrapping_add(needs.size);
            return range;
        }

        let count = self.blocks.len() as u32;
        for n in 0..count {
            let index = self.last.wrapping_add(n) % count;
            let block = &mut self.blocks[index as usize];
            if block.memory == vk::DeviceMemory::null()
                || block.ty != ty
                || BLOCK_BYTES.wrapping_sub(block.used) < needs.size
            {
                continue;
            }
            let Some(offset) = block.carve(needs.size, alignment) else { continue };
            range.offset = offset;
            range.memory = block.memory;
            range.block = index;
            self.last = index;
            self.totals.used_bytes = self.totals.used_bytes.wrapping_add(needs.size);
            return range;
        }

        let mut index = 0u32;
        while index < count && self.blocks[index as usize].memory != vk::DeviceMemory::null() {
            index += 1;
        }
        if index == count {
            self.blocks.push(Block::default());
        }
        // SAFETY: the caller's device.
        let memory = unsafe { self.next_block(vk, ty) };
        let block = &mut self.blocks[index as usize];
        block.memory = memory;
        if block.memory == vk::DeviceMemory::null() {
            *block = Block::default();
            return Range::default();
        }
        block.ty = ty;
        block.used = 0;
        block.free.clear();
        block.free.insert(0, BLOCK_BYTES);
        self.totals.blocks = self.totals.blocks.wrapping_add(1);
        self.totals.block_bytes = self.totals.block_bytes.wrapping_add(BLOCK_BYTES);
        // The carve's answer is taken unchecked: an aligned size past a fresh block
        // leaves offset 0 (needs.size is at most a quarter of a block, so it never is).
        range.offset = block.carve(needs.size, alignment).unwrap_or(0);
        range.memory = block.memory;
        range.block = index;
        self.last = index;
        self.totals.used_bytes = self.totals.used_bytes.wrapping_add(needs.size);
        // SAFETY: the caller's device.
        unsafe { self.ahead(vk, ty) };
        range
    }

    /// A block of type `ty` for allocate: the next one, made ahead or kept, when it is of
    /// that type (waited for if it is still being made), else made now. Null when the
    /// device has no memory left.
    ///
    /// # Safety
    /// As allocate's.
    unsafe fn next_block(&mut self, vk: &Vk, ty: u32) -> vk::DeviceMemory {
        let of_type = matches!(self.next, Some(Next::Making(t, _)) | Some(Next::Made(t, _)) if t == ty);
        if of_type {
            match self.next.take() {
                Some(Next::Made(_, memory)) => {
                    self.totals.taken_ahead = self.totals.taken_ahead.wrapping_add(1);
                    return memory;
                }
                Some(Next::Making(_, thread)) => {
                    let waited = !thread.is_finished();
                    let (memory, took) = thread.join().unwrap_or((vk::DeviceMemory::null(), 0));
                    if memory != vk::DeviceMemory::null() {
                        self.note_made(took);
                        self.totals.taken_ahead = self.totals.taken_ahead.wrapping_add(1);
                        if waited {
                            self.totals.waited_ahead = self.totals.waited_ahead.wrapping_add(1);
                        }
                        return memory;
                    }
                }
                None => {}
            }
        }
        let allocate =
            vk::MemoryAllocateInfo { allocation_size: BLOCK_BYTES, memory_type_index: ty, ..Default::default() };
        let began = crate::runtime::clock::nanoseconds();
        let mut memory = vk::DeviceMemory::null();
        // SAFETY: the caller's device.
        if unsafe { vk.allocate_memory(&allocate, &mut memory) } != vk::Result::SUCCESS {
            return vk::DeviceMemory::null();
        }
        let took = crate::runtime::clock::nanoseconds().wrapping_sub(began);
        self.note_made(took);
        self.totals.made_now = self.totals.made_now.wrapping_add(1);
        self.totals.made_now_nanos = self.totals.made_now_nanos.wrapping_add(took);
        #[cfg(feature = "diagnostics")]
        crate::log::log_info!("textures: a block of image memory made at a draw took %.1f ms", took as f64 / 1e6);
        memory
    }

    fn note_made(&mut self, took: u64) {
        self.totals.made = self.totals.made.wrapping_add(1);
        self.totals.making_nanos = self.totals.making_nanos.wrapping_add(took);
        self.totals.longest_nanos = self.totals.longest_nanos.max(took);
    }

    /// The next block of type `ty` started on the helper thread, when there is none. Not
    /// while the trace records the device's calls: it would miss the helper's.
    ///
    /// # Safety
    /// As allocate's.
    unsafe fn ahead(&mut self, vk: &Vk, ty: u32) {
        if self.next.is_some() || vk.traced() {
            return;
        }
        let device = vk.device.clone();
        let made = std::thread::Builder::new().name("image memory".into()).spawn(move || {
            let allocate =
                vk::MemoryAllocateInfo { allocation_size: BLOCK_BYTES, memory_type_index: ty, ..Default::default() };
            let began = crate::runtime::clock::nanoseconds();
            // SAFETY: the renderer's device, which outlives this thread (shutdown joins it);
            // vkAllocateMemory needs no lock of the caller's.
            let memory = unsafe { device.allocate_memory(&allocate, None) }.unwrap_or(vk::DeviceMemory::null());
            (memory, crate::runtime::clock::nanoseconds().wrapping_sub(began))
        });
        if let Ok(thread) = made {
            self.totals.made_ahead = self.totals.made_ahead.wrapping_add(1);
            self.next = Some(Next::Making(ty, thread));
        }
    }

    /// # Safety
    /// As allocate's, `range` one it gave and nothing on it still in use.
    pub unsafe fn free(&mut self, vk: &Vk, range: &mut Range) {
        if !range.is_some() {
            return;
        }
        if range.block == u32::MAX {
            // SAFETY: the caller's.
            unsafe { vk.free_memory(range.memory) };
            self.totals.alone = self.totals.alone.wrapping_sub(1);
            self.totals.alone_bytes = self.totals.alone_bytes.wrapping_sub(range.size);
            *range = Range::default();
            return;
        }
        let index = range.block as usize;
        let block = &mut self.blocks[index];
        let (mut start, mut size) = (range.offset, range.size);
        // The neighbour before, when it ends where this starts.
        if let Some((&before, &length)) = block.free.range(..start).next_back()
            && before.wrapping_add(length) == start
        {
            start = before;
            size = size.wrapping_add(length);
            block.free.remove(&before);
        }
        // The one after (the first at or past the range's own start), when this ends there.
        if let Some((&next, &length)) = block.free.range(range.offset..).next()
            && start.wrapping_add(size) == next
        {
            size = size.wrapping_add(length);
            block.free.remove(&next);
        }
        block.free.insert(start, size);
        block.used = block.used.wrapping_sub(range.size);
        self.totals.used_bytes = self.totals.used_bytes.wrapping_sub(range.size);
        *range = Range::default();

        // An empty block goes back to the device when another of its type has room for
        // what comes next, so a level's worth of blocks is not kept for the menu.
        if block.used != 0 {
            return;
        }
        let ty = block.ty;
        let room = self.blocks.iter().enumerate().any(|(n, other)| {
            n != index
                && other.memory != vk::DeviceMemory::null()
                && other.ty == ty
                && BLOCK_BYTES.wrapping_sub(other.used) >= BLOCK_BYTES / 4
        });
        if room {
            let block = &mut self.blocks[index];
            // Kept as the next block when there is none: making one again costs a frame.
            if self.next.is_none() {
                self.next = Some(Next::Made(ty, block.memory));
                *block = Block::default();
                self.totals.kept = self.totals.kept.wrapping_add(1);
                self.totals.blocks = self.totals.blocks.wrapping_sub(1);
                self.totals.block_bytes = self.totals.block_bytes.wrapping_sub(BLOCK_BYTES);
                return;
            }
            // SAFETY: the caller's; nothing is left on the block.
            unsafe { vk.free_memory(block.memory) };
            *block = Block::default();
            self.totals.given_back = self.totals.given_back.wrapping_add(1);
            self.totals.blocks = self.totals.blocks.wrapping_sub(1);
            self.totals.block_bytes = self.totals.block_bytes.wrapping_sub(BLOCK_BYTES);
        }
    }

    /// Every block, with the device idle and every image on them destroyed.
    ///
    /// # Safety
    /// As allocate's.
    pub unsafe fn shutdown(&mut self, vk: &Vk) {
        let next = match self.next.take() {
            Some(Next::Making(_, thread)) => thread.join().map_or(vk::DeviceMemory::null(), |(memory, _)| memory),
            Some(Next::Made(_, memory)) => memory,
            None => vk::DeviceMemory::null(),
        };
        if next != vk::DeviceMemory::null() {
            // SAFETY: the caller's; nothing was ever put on it.
            unsafe { vk.free_memory(next) };
        }
        for block in &self.blocks {
            if block.memory != vk::DeviceMemory::null() {
                // SAFETY: the caller's.
                unsafe { vk.free_memory(block.memory) };
            }
        }
        self.blocks.clear();
        self.last = 0;
        // What is held goes; what the run made stays for the report after the shutdown.
        let t = self.totals;
        self.totals = Totals {
            made: t.made,
            given_back: t.given_back,
            making_nanos: t.making_nanos,
            longest_nanos: t.longest_nanos,
            alone_made: t.alone_made,
            made_ahead: t.made_ahead,
            taken_ahead: t.taken_ahead,
            waited_ahead: t.waited_ahead,
            kept: t.kept,
            made_now: t.made_now,
            made_now_nanos: t.made_now_nanos,
            ..Totals::default()
        };
    }

    pub fn held(&self) -> Totals {
        self.totals
    }
}
