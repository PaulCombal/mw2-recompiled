// A texture the title keeps rewriting a little at a time, read again in parts.
//
// MW2 rewrites its model-lighting table, a 512x256x4 8_8_8_8 volume of 2 MB, every
// frame, a 4x4 patch for each model it lights: about 1% of its pages between two reads.
// Read whole, as the cache reads any texture that changed, it was 84% of the time the
// ring consumer spent reading textures in Team Player. So a texture the watch covers,
// once it has changed, is cut into parts -- a 32x32-block tile of a level (32x16x4 for a
// volume, whose tiles are that shape) -- each with the guest pages it is read from and
// a signature of its bytes. At a re-read, only the parts with a page written since the
// last are read, and only those whose bytes changed are uploaded, into the image as it
// is. The bytes of a part are read by the same walk as the whole texture's
// (texture::read_region), so a part reads what a whole read would put there.
//
// The watch decides which parts are read and the signature, as for a whole texture,
// whether one changed. A copy of the bytes as uploaded is kept beside them, so that
// whether the texture is still mostly empty is judged as after a whole read. A page
// written by something the watch does not see would leave its part stale:
// MW2_CHECK_TEXTURE_PARTS=1 (diagnostics) reads the whole texture after each re-read in
// parts and counts the bytes that differ from what was uploaded.

use super::super::texture::{self, BlockRect, Level, TextureFetch};
use ash::vk;

/// The watch's page, which a part's pages are counted in.
pub const PAGE: u32 = crate::runtime::gpu::memory_watch::PAGE_BYTES;

/// One part: a box of one level's blocks, the pages it is read from (indices into
/// Parts::pages), and the signature of its bytes as last uploaded.
#[derive(Clone)]
pub struct Part {
    pub level: u32,
    pub rect: BlockRect,
    pages: std::ops::Range<u32>,
    pub signature: u64,
    /// Its bytes: (x1 - x0) blocks a row, row after row, layer after layer.
    pub bytes: usize,
}

#[derive(Clone)]
pub struct Parts {
    pub list: Vec<Part>,
    // Every part's pages, a part's after the one before's, as indices into `pages`.
    page_of_part: Vec<u32>,
    /// The distinct guest pages (physical, in 4 KB pages) the texture is read from.
    pub pages: Vec<u32>,
    /// The texture as uploaded, in read_texture's layout: what note_emptiness looks at,
    /// and MW2_CHECK_TEXTURE_PARTS compares with a whole read.
    pub mirror: Vec<u8>,
    /// How many of `mirror`'s 64-byte spans are all zero, kept as parts are placed: what
    /// empty_percent would count over it.
    pub empty_spans: usize,
    /// MW2_CHECK_TEXTURE_PARTS: what the copies of the parts have put in the image, worked
    /// out from their regions (apply_copy), to compare with `mirror`.
    pub copied: Vec<u8>,
    /// Its re-reads in parts, the parts they read and the parts they uploaded.
    pub rereads: u64,
    pub read: u64,
    pub uploaded: u64,
}

/// The shape of a part along each axis, in blocks: a tile.
fn tile_of(volume: bool) -> (u32, u32, u32) {
    if volume { (32, 16, 4) } else { (32, 32, 1) }
}

/// The parts of a texture: every level read (level 0 when the base is, the others when
/// the chain is), cut into tiles. `base` and `mips` are the physical addresses of the two
/// ranges, zero when that range is not read.
pub fn cut(fetch: &TextureFetch, levels: &[Level], base: u32, mips: u32, bytes_per_block: u32, volume: bool) -> Parts {
    let (tile_x, tile_y, tile_z) = tile_of(volume);
    let mut list = Vec::new();
    let mut page_of_part = Vec::new();
    let mut seen: Vec<u32> = Vec::new();
    for (level, l) in levels.iter().enumerate() {
        let read = if level == 0 { base != 0 } else { mips != 0 };
        if !read {
            continue;
        }
        let depth = if volume { l.layers } else { 1 };
        let layers = if volume { 1 } else { l.layers };
        for layer in 0..layers {
            for z in (0..depth).step_by(tile_z as usize) {
                for y in (0..l.blocks_high).step_by(tile_y as usize) {
                    for x in (0..l.blocks_wide).step_by(tile_x as usize) {
                        let rect = BlockRect {
                            x0: x,
                            x1: (x + tile_x).min(l.blocks_wide),
                            y0: y,
                            y1: (y + tile_y).min(l.blocks_high),
                            z0: if volume { z } else { layer },
                            z1: if volume { (z + tile_z).min(depth) } else { layer + 1 },
                        };
                        let first = page_of_part.len() as u32;
                        texture::region_runs(fetch, level as u32, rect, |in_chain, offset, bytes| {
                            let start = (if in_chain { mips } else { base }).wrapping_add(offset as u32);
                            for page in (start / PAGE)..=(start.wrapping_add(bytes as u32 - 1) / PAGE) {
                                seen.push(page);
                                page_of_part.push(page);
                            }
                        });
                        // Each part's pages once.
                        let mine = &mut page_of_part[first as usize..];
                        mine.sort_unstable();
                        let kept = {
                            let mut kept = 0;
                            for i in 0..mine.len() {
                                if i == 0 || mine[i] != mine[kept - 1] {
                                    mine[kept] = mine[i];
                                    kept += 1;
                                }
                            }
                            kept
                        };
                        page_of_part.truncate(first as usize + kept);
                        let bytes = (rect.x1 - rect.x0) as usize
                            * (rect.y1 - rect.y0) as usize
                            * (rect.z1 - rect.z0) as usize
                            * bytes_per_block as usize;
                        list.push(Part {
                            level: level as u32,
                            rect,
                            pages: first..page_of_part.len() as u32,
                            signature: 0,
                            bytes,
                        });
                    }
                }
            }
        }
    }
    seen.sort_unstable();
    seen.dedup();
    // Pages as indices into the distinct list.
    for page in &mut page_of_part {
        *page = seen.binary_search(page).unwrap_or(0) as u32;
    }
    Parts { list, page_of_part, pages: seen, mirror: Vec::new(), empty_spans: 0, copied: Vec::new(), rereads: 0, read: 0, uploaded: 0 }
}

impl Parts {
    /// Whether any of part `i`'s pages is marked in `written` (one flag a page of `pages`).
    pub fn touched(&self, i: usize, written: &[bool]) -> bool {
        let part = &self.list[i];
        self.page_of_part[part.pages.start as usize..part.pages.end as usize].iter().any(|&p| written[p as usize])
    }
}

/// Where part `part` goes in the image: its region of a buffer-to-image copy, its bytes at
/// `offset` in the buffer.
pub fn region(part: &Part, l: &Level, block_width: u32, block_height: u32, volume: bool, offset: u64) -> vk::BufferImageCopy {
    let r = part.rect;
    let (x, y) = (r.x0 * block_width, r.y0 * block_height);
    vk::BufferImageCopy {
        buffer_offset: offset,
        buffer_row_length: (r.x1 - r.x0) * block_width,
        buffer_image_height: (r.y1 - r.y0) * block_height,
        image_subresource: vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: part.level,
            base_array_layer: if volume { 0 } else { r.z0 },
            layer_count: if volume { 1 } else { r.z1 - r.z0 },
        },
        image_offset: vk::Offset3D { x: x as i32, y: y as i32, z: if volume { r.z0 as i32 } else { 0 } },
        // A block format's edge part may run past the level; the copy ends at its edge.
        image_extent: vk::Extent3D {
            width: ((r.x1 - r.x0) * block_width).min(l.width.saturating_sub(x)),
            height: ((r.y1 - r.y0) * block_height).min(l.height.saturating_sub(y)),
            depth: if volume { r.z1 - r.z0 } else { 1 },
        },
    }
}

/// The 64-byte spans empty_percent counts over `bytes`, and how many are all zero.
pub fn empty_spans(bytes: &[u8]) -> usize {
    bytes.chunks_exact(64).filter(|span| span.iter().all(|&b| b == 0)).count()
}

/// empty_percent's answer from Parts::empty_spans.
pub fn empty_percent(parts: &Parts) -> u8 {
    let spans = parts.mirror.len() / 64;
    if spans == 0 {
        return 0;
    }
    (parts.empty_spans * 100 / spans) as u8
}

/// Each row of part `part` in read_texture's layout: (where it starts, its bytes).
fn rows_of(part: &Part, l: &Level, bytes_per_block: u32, mut row: impl FnMut(usize, usize, usize)) {
    let r = part.rect;
    let bpb = bytes_per_block as usize;
    let bytes = (r.x1 - r.x0) as usize * bpb;
    let rows = (r.y1 - r.y0) as usize;
    let level_row = l.blocks_wide as usize * bpb;
    for z in r.z0..r.z1 {
        for y in r.y0..r.y1 {
            let from = ((z - r.z0) as usize * rows + (y - r.y0) as usize) * bytes;
            let to = l.offset + (z as usize * l.blocks_high as usize + y as usize) * level_row + r.x0 as usize * bpb;
            row(from, to, bytes);
        }
    }
}

/// Part `part`'s bytes put into `whole`, laid out as read_texture lays the texture out.
pub fn place(part: &Part, l: &Level, bytes_per_block: u32, bytes: &[u8], whole: &mut [u8]) {
    rows_of(part, l, bytes_per_block, |from, to, n| whole[to..to + n].copy_from_slice(&bytes[from..from + n]));
}

/// place into the parts' copy of the texture, its count of empty spans kept: the spans
/// the part's rows touch counted before and after.
pub fn place_counted(parts: &mut Parts, i: usize, l: &Level, bytes_per_block: u32, bytes: &[u8], spans: &mut Vec<usize>) {
    let whole_spans = parts.mirror.len() / 64;
    spans.clear();
    rows_of(&parts.list[i], l, bytes_per_block, |_, to, n| spans.extend((to / 64)..((to + n).div_ceil(64)).min(whole_spans)));
    spans.sort_unstable();
    spans.dedup();
    let empty = |mirror: &[u8], spans: &[usize]| {
        spans.iter().filter(|&&s| mirror[s * 64..s * 64 + 64].iter().all(|&b| b == 0)).count()
    };
    let before = empty(&parts.mirror, spans);
    place(&parts.list[i], l, bytes_per_block, bytes, &mut parts.mirror);
    let after = empty(&parts.mirror, spans);
    parts.empty_spans = parts.empty_spans + after - before;
}

/// Part `part`'s bytes taken out of `whole`, read_texture's layout, into `bytes`.
pub fn take(part: &Part, l: &Level, bytes_per_block: u32, whole: &[u8], bytes: &mut [u8]) {
    let r = part.rect;
    let bpb = bytes_per_block as usize;
    let row = (r.x1 - r.x0) as usize * bpb;
    let rows = (r.y1 - r.y0) as usize;
    let level_row = l.blocks_wide as usize * bpb;
    for z in r.z0..r.z1 {
        for y in r.y0..r.y1 {
            let to = ((z - r.z0) as usize * rows + (y - r.y0) as usize) * row;
            let from = l.offset + (z as usize * l.blocks_high as usize + y as usize) * level_row + r.x0 as usize * bpb;
            bytes[to..to + row].copy_from_slice(&whole[from..from + row]);
        }
    }
}

/// MW2_CHECK_TEXTURE_PARTS: the regions of a buffer-to-image copy applied to `image`, the
/// texture laid out as read_texture lays it out, as the Vulkan specification has a copy
/// address its buffer and its image ("Copying Data Between Buffers and Images"): what the
/// copy of the parts puts where, worked out apart from how the parts were cut.
pub fn apply_copy(
    regions: &[vk::BufferImageCopy],
    buffer: &[u8],
    levels: &[Level],
    block_width: u32,
    block_height: u32,
    bytes_per_block: u32,
    volume: bool,
    image: &mut [u8],
) {
    let bpb = bytes_per_block as usize;
    for r in regions {
        let l = &levels[r.image_subresource.mip_level as usize];
        let row_blocks = r.buffer_row_length.div_ceil(block_width) as usize;
        let rows_per_slice = r.buffer_image_height.div_ceil(block_height) as usize;
        let (wide, high) = (r.image_extent.width.div_ceil(block_width), r.image_extent.height.div_ceil(block_height));
        let (x0, y0) = (r.image_offset.x as u32 / block_width, r.image_offset.y as u32 / block_height);
        let slices = if volume { r.image_extent.depth } else { r.image_subresource.layer_count };
        for slice in 0..slices {
            let layer = if volume { r.image_offset.z as u32 + slice } else { r.image_subresource.base_array_layer + slice };
            for y in 0..high {
                for x in 0..wide {
                    let from = r.buffer_offset as usize + ((slice as usize * rows_per_slice + y as usize) * row_blocks + x as usize) * bpb;
                    let to = l.offset
                        + ((layer as usize * l.blocks_high as usize) + (y0 + y) as usize) * l.blocks_wide as usize * bpb
                        + (x0 + x) as usize * bpb;
                    image[to..to + bpb].copy_from_slice(&buffer[from..from + bpb]);
                }
            }
        }
    }
}
