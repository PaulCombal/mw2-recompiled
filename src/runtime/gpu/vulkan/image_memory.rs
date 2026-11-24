// Memory for the texture cache's images, carved out of large blocks.
//
// A level streams tens of thousands of textures through the cache, most of them a few
// kilobytes. An allocation of its own for each is what a driver is worst at: freeing one
// took over half a millisecond with forty thousand live, and some drivers allow only 4096
// at all. A block is one allocation of BLOCK_BYTES of one memory type, and a texture gets
// a range of it, so releasing a texture is bookkeeping and never a call into the driver.
// What does not fit a quarter of a block gets an allocation of its own.
//
// One thread: the ring consumer, which owns the texture cache that owns this.
#![deny(unsafe_op_in_unsafe_fn)]

use super::device::Vk;
use ash::vk;
use std::collections::BTreeMap;

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

#[derive(Default)]
pub struct ImageMemory {
    blocks: Vec<Block>, // a freed block's entry is kept, empty, and reused
    last: u32,          // the block the last range came from
    totals: Totals,
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
        let block = &mut self.blocks[index as usize];
        let allocate =
            vk::MemoryAllocateInfo { allocation_size: BLOCK_BYTES, memory_type_index: ty, ..Default::default() };
        // SAFETY: the caller's device.
        if unsafe { vk.allocate_memory(&allocate, &mut block.memory) } != vk::Result::SUCCESS {
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
        range
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
            // SAFETY: the caller's; nothing is left on the block.
            unsafe { vk.free_memory(block.memory) };
            *block = Block::default();
            self.totals.blocks = self.totals.blocks.wrapping_sub(1);
            self.totals.block_bytes = self.totals.block_bytes.wrapping_sub(BLOCK_BYTES);
        }
    }

    /// Every block, with the device idle and every image on them destroyed.
    ///
    /// # Safety
    /// As allocate's.
    pub unsafe fn shutdown(&mut self, vk: &Vk) {
        for block in &self.blocks {
            if block.memory != vk::DeviceMemory::null() {
                // SAFETY: the caller's.
                unsafe { vk.free_memory(block.memory) };
            }
        }
        self.blocks.clear();
        self.last = 0;
        self.totals = Totals::default();
    }

    pub fn held(&self) -> Totals {
        self.totals
    }
}
