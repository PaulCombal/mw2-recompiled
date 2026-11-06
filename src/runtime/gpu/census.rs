// What the command stream holds, counted: draws and primitives, presents and draws per
// second, a frame's surfaces and resolves in order -- and the texture dumps, which are
// taken where the draws are (runtime/gpu/census.cpp). Diagnostics only.
//
// The counting is single-writer atomics (the consumer's draws, a guest thread's
// presents); the dumps read guest memory through the untiler (gpu/texture.rs), whose
// contract this keeps: the ranges are the guest's, mapped whole.

use super::registers::{self as r, RegisterFile};
use super::texture::{self, DIMENSION_3D, Decoded, TextureFetch};
use crate::log::{format_text, log_info, log_kernel, log_warn};
use crate::runtime::kernel;
use std::collections::HashSet;
use std::ffi::CStr;
use std::io::Write;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{LazyLock, Mutex};

const SECONDS_TRACKED: usize = 400;

struct Counts {
    draws: AtomicU64,
    draws_indexed: AtomicU64,
    draws_auto: AtomicU64,
    draw_indices: AtomicU64,
    primitive_count: [AtomicU64; 64],
    // Draws are counted beside presents because a run that presents 60 times a second
    // over a menu and one that presents 60 times a second over a level are the same
    // number and not the same measurement.
    presents_by_second: [AtomicU64; SECONDS_TRACKED],
    draws_by_second: [AtomicU64; SECONDS_TRACKED],
    draws_this_frame: AtomicU64,
    presents: AtomicU64,
    last_present_ms: AtomicI64,
    // The render target set-up the frame trace last printed.
    last_surface: [AtomicU32; 4],
}

static G: Counts = Counts {
    draws: AtomicU64::new(0),
    draws_indexed: AtomicU64::new(0),
    draws_auto: AtomicU64::new(0),
    draw_indices: AtomicU64::new(0),
    primitive_count: [const { AtomicU64::new(0) }; 64],
    presents_by_second: [const { AtomicU64::new(0) }; SECONDS_TRACKED],
    draws_by_second: [const { AtomicU64::new(0) }; SECONDS_TRACKED],
    draws_this_frame: AtomicU64::new(0),
    presents: AtomicU64::new(0),
    last_present_ms: AtomicI64::new(-1),
    last_surface: [const { AtomicU32::new(0) }; 4],
};

// counter.h's Counter: one writer, so a load and a store.
fn add(counter: &AtomicU64, n: u64) {
    counter.store(counter.load(Relaxed).wrapping_add(n), Relaxed);
}

fn seconds() -> f64 {
    super::gpu::seconds()
}

fn second_now() -> Option<usize> {
    let seconds = seconds();
    (seconds >= 0.0 && seconds < SECONDS_TRACKED as f64).then_some(seconds as usize)
}

// MW2_TRACE_FRAME=<n>: n frames' surfaces and resolves, in order, from
// MW2_TRACE_DRAWS_AFTER presents on: what shape the frame graph has.
fn tracing_frame() -> bool {
    static FRAMES: LazyLock<u64> = LazyLock::new(|| crate::env::number(c"MW2_TRACE_FRAME", 0));
    static AFTER: LazyLock<u64> = LazyLock::new(|| crate::env::number(c"MW2_TRACE_DRAWS_AFTER", 0));
    let presents = G.presents.load(Relaxed);
    *FRAMES != 0 && presents >= *AFTER && presents < AFTER.wrapping_add(*FRAMES)
}

fn scissor_extent(registers: &RegisterFile) -> (u32, u32) {
    let top_left = r::ScissorCorner(registers.get(r::PA_SC_WINDOW_SCISSOR_TL));
    let bottom_right = r::ScissorCorner(registers.get(r::PA_SC_WINDOW_SCISSOR_BR));
    let width = if bottom_right.x() > top_left.x() { bottom_right.x() - top_left.x() } else { 0 };
    let height = if bottom_right.y() > top_left.y() { bottom_right.y() - top_left.y() } else { 0 };
    (width, height)
}

fn trace_surface(registers: &RegisterFile, mode: r::EdramMode) {
    let key =
        [registers.get(r::RB_SURFACE_INFO), registers.get(r::RB_COLOR_INFO), registers.get(r::RB_DEPTH_INFO), mode.0];
    if key.iter().zip(&G.last_surface).all(|(k, last)| *k == last.load(Relaxed)) {
        return;
    }
    for (k, last) in key.iter().zip(&G.last_surface) {
        last.store(*k, Relaxed);
    }
    let surface = r::SurfaceInfo(key[0]);
    let colour = r::ColorInfo(key[1]);
    let depth = r::DepthInfo(key[2]);
    let (width, height) = scissor_extent(registers);
    log_kernel!(
        "surface: pitch %u, %ux MSAA, colour %s at tile %u, depth %s at tile %u, %s, scissor %ux%u",
        surface.pitch(),
        1u32 << surface.msaa_samples(),
        r::color_target_format_name(colour.format()).unwrap_or(c"unknown"),
        colour.base_tile(),
        r::depth_target_format_name(depth.format()),
        depth.base_tile(),
        mode.name(),
        width,
        height
    );
}

// A draw in copy mode is a resolve: the rectangle says which part of EDRAM to copy out,
// RB_COPY_* where it lands.
fn trace_resolve(registers: &RegisterFile) {
    let control = r::CopyControl(registers.get(r::RB_COPY_CONTROL));
    let info = r::CopyDestInfo(registers.get(r::RB_COPY_DEST_INFO));
    let pitch = r::CopyDestPitch(registers.get(r::RB_COPY_DEST_PITCH));
    let (width, height) = scissor_extent(registers);
    let destination = registers.get(r::RB_COPY_DEST_BASE);
    let command = control.command();
    log_kernel!(
        "resolve: %s -> %08X (%08X), %ux%u of a %ux%u surface, format %u, endian %u, %s%s%s",
        if control.copies_depth() { c"depth" } else { c"colour" },
        destination,
        kernel::from_physical(destination),
        width,
        height,
        pitch.pitch(),
        pitch.height(),
        info.format(),
        info.endian(),
        match command {
            0 => c"raw",
            1 => c"converting",
            _ => c"other",
        },
        if control.clears_color() { c", clears colour" } else { c"" },
        if control.clears_depth() { c", clears depth" } else { c"" }
    );
}

// MW2_DUMP_TEXTURES=<dir>: the only way to confirm by eye that untiling, endianness,
// format and addressing are all right at once -- a wrong tiling function still produces
// a plausible buffer, but not a picture.
fn dump_texture_directory() -> Option<&'static CStr> {
    static DIR: LazyLock<Option<&'static CStr>> = LazyLock::new(|| crate::env::text(c"MW2_DUMP_TEXTURES"));
    *DIR
}

fn create(path: &CStr) -> Option<std::fs::File> {
    std::fs::File::create(crate::runtime::platform::system_path(path)).ok()
}

fn dump_texture(fetch: &TextureFetch) {
    let Some(dir) = dump_texture_directory() else { return };
    let name = texture::texture_format_name(fetch.format()).unwrap_or(c"unknown");
    let va = kernel::from_physical(fetch.base_address());
    if va == 0 {
        log_warn!(
            "texture: %s %ux%u at physical %08X does not resolve",
            name,
            fetch.width(),
            fetch.height(),
            fetch.base_address()
        );
        return;
    }
    let base = crate::runtime::guest_memory::base();
    // MW2_DUMP_TEXTURES_RAW=1: the bytes as the guest holds them, before any untiling,
    // so a layout can be tried offline instead of costing a run per guess. Always for a
    // volume.
    static RAW: LazyLock<bool> = LazyLock::new(|| crate::env::flag(c"MW2_DUMP_TEXTURES_RAW"));
    if fetch.dimension() == DIMENSION_3D || *RAW {
        static SEQUENCE: AtomicU32 = AtomicU32::new(0);
        let sequence = SEQUENCE.fetch_add(1, Relaxed);
        let path = format_text!(
            "%s/raw_%s_%ux%ux%u_at_%08X_%03u.bin",
            dir,
            name,
            fetch.width(),
            fetch.height(),
            fetch.depth(),
            fetch.base_address(),
            sequence
        );
        let info = texture::texture_format_of(fetch.format());
        let block_width = if info.block_width != 0 { info.block_width } else { 1 };
        let block_height = if info.block_height != 0 { info.block_height } else { 1 };
        let bytes = if fetch.dimension() == DIMENSION_3D {
            (fetch.pitch() as usize)
                .wrapping_mul((fetch.height().wrapping_add(31) & !31) as usize)
                .wrapping_mul(fetch.depth() as usize)
                .wrapping_mul(4)
        } else {
            ((fetch.pitch() / block_width).wrapping_add(31) & !31) as usize
                * ((fetch.height().wrapping_add(block_height - 1) / block_height).wrapping_add(31) & !31) as usize
                * (info.bits_per_block / 8) as usize
        };
        if let Some(mut file) = create(&path) {
            let mut copy = vec![0u8; bytes];
            // SAFETY: guest memory, mapped whole; copied out rather than viewed as a slice.
            unsafe { std::ptr::copy_nonoverlapping(base.wrapping_add(va as usize), copy.as_mut_ptr(), bytes) };
            let _ = file.write_all(&copy);
            drop(file);
            log_info!("texture: wrote %s (%llu bytes, tiled as the guest holds them)", path.as_c_str(), bytes as u64);
        }
    }
    let extents = texture::extents_of(fetch);
    let mip_va = if extents.mip_address != 0 { kernel::from_physical(extents.mip_address) } else { 0 };
    let mut bytes: Vec<u8> = Vec::new();
    // SAFETY: the guest's ranges, mapped whole (read_texture's contract: a page never
    // touched is committed by the fault handler); `reserve` gives zeroed bytes it owns.
    let data = unsafe {
        texture::read_texture(
            fetch,
            base.wrapping_add(va as usize),
            if mip_va != 0 { base.wrapping_add(mip_va as usize) } else { std::ptr::null() },
            &mut |size| {
                bytes.clear();
                bytes.resize(size, 0);
                bytes.as_mut_ptr()
            },
        )
    };
    if !data.ok {
        // SAFETY: a failed read's reason is a static string.
        let error = if data.error.is_null() { c"" } else { unsafe { CStr::from_ptr(data.error) } };
        log_warn!("texture: %s %ux%u not read: %s", name, fetch.width(), fetch.height(), error);
        return;
    }
    let levels = &data.levels[..(data.level_count as usize).min(data.levels.len())];
    let decoded =
        Decoded { ok: data.ok, expanded: data.expanded, bytes_per_block: data.bytes_per_block, levels, bytes: &bytes };
    let mut rgba: Vec<u8> = Vec::new();
    let decode = |level: u32, rgba: &mut Vec<u8>| {
        // SAFETY: the closure gives a zeroed buffer this owns.
        unsafe {
            texture::decode_to_rgba(&decoded, fetch.format(), level, &mut |size| {
                rgba.clear();
                rgba.resize(size, 0);
                rgba.as_mut_ptr()
            })
        }
    };
    if !decode(0, &mut rgba) {
        log_info!(
            "texture: %s %ux%u read (%llu bytes) but not decodable for display",
            name,
            data.width,
            data.height,
            bytes.len() as u64
        );
        return;
    }
    // Blank in guest memory means the title has not filled it, which no untiling
    // changes -- worth distinguishing from a bad read.
    let non_zero = bytes.iter().filter(|&&b| b != 0).count();
    const DIMENSIONS: [&CStr; 4] = [c"1D", c"2D", c"3D", c"cube"];
    log_info!(
        "texture: %s %ux%u %s at physical %08X (%08X), pitch %u, %s, %llu%% of %llu bytes non-zero, %u layers",
        name,
        data.width,
        data.height,
        DIMENSIONS[(fetch.dimension() & 3) as usize],
        fetch.base_address(),
        va,
        fetch.pitch(),
        if fetch.tiled() { c"tiled" } else { c"linear" },
        if bytes.is_empty() { 0 } else { (non_zero * 100 / bytes.len()) as u64 },
        bytes.len() as u64,
        data.layers
    );
    // A streamed texture fills its small levels first and says so with a min mip above
    // zero and a separate address for the chain.
    log_info!(
        "texture:   mips %u..%u, chain at %08X%s; its six fetch dwords: %08X %08X %08X %08X %08X %08X",
        fetch.min_mip_level(),
        fetch.max_mip_level(),
        fetch.mip_address(),
        if fetch.packed_mips() { c", packed" } else { c"" },
        fetch.d[0],
        fetch.d[1],
        fetch.d[2],
        fetch.d[3],
        fetch.d[4],
        fetch.d[5]
    );
    // Every slice, stacked: a volume whose first slice is blank and whose rest is not
    // looks exactly like a volume that read as nothing. Each level of a chain after the
    // first goes in a file of its own.
    for (level, l) in levels.iter().enumerate() {
        if level != 0 && !decode(level as u32, &mut rgba) {
            break;
        }
        let path = if level != 0 {
            format_text!(
                "%s/%s_%ux%u_at_%08X_level%u.pnm",
                dir,
                name,
                data.width,
                data.height,
                fetch.base_address(),
                level as u32
            )
        } else {
            format_text!("%s/%s_%ux%u_at_%08X.pnm", dir, name, data.width, data.height, fetch.base_address())
        };
        let Some(mut file) = create(&path) else { break };
        let header = format_text!("P6\n%u %u\n255\n", l.width, l.height.wrapping_mul(l.layers));
        let mut out = header.as_bytes().to_vec();
        for pixel in rgba.chunks(4) {
            out.extend_from_slice(&pixel[..3.min(pixel.len())]);
        }
        let _ = file.write_all(&out);
        drop(file);
        if level == 0 {
            log_info!("texture: wrote %s", path.as_c_str());
        }
    }
}

pub fn draw(registers: &RegisterFile, primitive: u32, index_count: u32, indexed: bool, mode: r::EdramMode) {
    add(&G.draws, 1);
    add(&G.draws_this_frame, 1);
    add(&G.primitive_count[(primitive & 63) as usize], 1);
    add(&G.draw_indices, u64::from(index_count));
    add(if indexed { &G.draws_indexed } else { &G.draws_auto }, 1);
    if tracing_frame() {
        if mode == r::EdramMode::COPY {
            trace_resolve(registers);
        } else {
            trace_surface(registers, mode);
        }
    }
}

/// At each swap packet the parse meets.
pub fn swap() {
    if let Some(second) = second_now().and_then(|second| G.draws_by_second.get(second)) {
        second.fetch_add(G.draws_this_frame.load(Relaxed), Relaxed);
    }
    G.draws_this_frame.store(0, Relaxed);
}

/// At each present the title makes.
pub fn present() {
    G.presents.fetch_add(1, Relaxed);
    G.last_present_ms.store((seconds() * 1000.0) as i64, Relaxed);
    if let Some(second) = second_now().and_then(|second| G.presents_by_second.get(second)) {
        second.fetch_add(1, Relaxed);
    }
}

/// MW2_DUMP_TEXTURES: every distinct texture shape bound, once.
pub fn dumping_textures() -> bool {
    dump_texture_directory().is_some()
}

// Each distinct texture shape the title binds, once. A texture the title streams in is
// empty at its first bind and full a second later, which dumping on first sight cannot
// tell from one that never arrives, so MW2_DUMP_TEXTURES_AFTER=<seconds> waits.
#[cold]
#[inline(never)]
pub fn dump_bound_textures(registers: &RegisterFile) {
    static AFTER: LazyLock<f64> = LazyLock::new(|| crate::env::real(c"MW2_DUMP_TEXTURES_AFTER", 0.0));
    static DUMPED: Mutex<Option<HashSet<u64>>> = Mutex::new(None);
    if seconds() < *AFTER {
        return;
    }
    for slot in 0..32 {
        let mut fetch = TextureFetch::default();
        for (i, d) in fetch.d.iter_mut().enumerate() {
            *d = registers.get(r::CONSTANT_BASE_FETCH + slot * 6 + i as u32);
        }
        if !fetch.is_texture() || fetch.base_address() == 0 {
            continue;
        }
        let shape = (u64::from(fetch.sign(0)) << 46)
            | (u64::from(fetch.format()) << 40)
            | (u64::from(fetch.dimension()) << 38)
            | (u64::from(fetch.tiled()) << 37)
            | (u64::from(fetch.endian()) << 35)
            | (u64::from(fetch.width() & 0x1FFF) << 22)
            | (u64::from(fetch.height() & 0x1FFF) << 9)
            | u64::from(fetch.max_mip_level() & 0xF);
        let fresh = DUMPED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_or_insert_with(HashSet::new)
            .insert(shape);
        if fresh {
            dump_texture(&fetch);
        }
    }
}

pub fn report() {
    log_info!("gpu: last present at %lld ms", G.last_present_ms.load(Relaxed));
    let last = (0..SECONDS_TRACKED).filter(|&i| G.presents_by_second[i].load(Relaxed) != 0).max().unwrap_or(0);
    // Twenty seconds to a line, labelled, so a rate that changes partway through a run
    // can be read off against the wall clock that drove it.
    for from in (0..=last).step_by(20) {
        let mut line = Vec::new();
        for i in from..(from + 20).min(last + 1) {
            line.extend_from_slice(format_text!("%3llu ", G.presents_by_second[i].load(Relaxed)).as_bytes());
        }
        let line = crate::log::c_string(line);
        log_info!("gpu: presents per second %3ds: %s", from as i32, line.as_c_str());
    }
    for from in (0..=last).step_by(20) {
        let mut line = Vec::new();
        for i in from..(from + 20).min(last + 1) {
            line.extend_from_slice(format_text!("%4llu ", G.draws_by_second[i].load(Relaxed) / 1000).as_bytes());
        }
        let line = crate::log::c_string(line);
        log_info!("gpu: thousands of draws per second %3ds: %s", from as i32, line.as_c_str());
    }

    let frames = super::gpu::frame_count().max(1);
    let draws = G.draws.load(Relaxed);
    log_info!(
        "gpu: %llu draws (%.1f per frame): %llu indexed, %llu auto, %llu indices total",
        draws,
        draws as f64 / frames as f64,
        G.draws_indexed.load(Relaxed),
        G.draws_auto.load(Relaxed),
        G.draw_indices.load(Relaxed)
    );
    if draws != 0 {
        let mut line = Vec::new();
        for (i, count) in G.primitive_count.iter().enumerate() {
            let v = count.load(Relaxed);
            if v == 0 {
                continue;
            }
            let one = match r::primitive_name(i as u32) {
                Some(name) => format_text!("%s %llu; ", name, v),
                None => format_text!("type %u (unknown) %llu; ", i as u32, v),
            };
            line.extend_from_slice(one.as_bytes());
        }
        let line = crate::log::c_string(line);
        log_info!("gpu: primitives: %s", line.as_c_str());
    }
}
