// The switches that look at what the renderer drew -- frame and target dumps, the flash
// recorder, the traces -- and the report. Each is a diagnostics build's: a release build
// compiles them out.
//
// A dump's buffer is sized by its format's texel, so the copy of a 16_16_16_16,
// 16_16_16_16_FLOAT or 32_32_FLOAT surface (eight bytes a texel) fits it; such a dump is
// written as eight bits a channel.
//
// Unsafe by nature: the dumps record into the set-up command buffer and read mapped memory
// once its fence says the copy is done; the read-back buffer is copied into inside the
// frame's submission, which a frame read back is waited for.
#![deny(unsafe_op_in_unsafe_fn)]

use super::device::Vk;
use super::recorder;
use super::renderer_arena::DrawNotes;
use super::renderer_frame::{Core, FrameView};
use super::renderer_state::{
    ARENA_KINDS, ARENA_NAMES, PA_SU_SC_MODE_CNTL, RB_BLENDCONTROL0, RB_COLOR_MASK, RB_DEPTHCONTROL, ResolveRecords,
    TargetKey,
};
use super::renderer_targets::{DEPTH_ASPECTS, Targets};
use super::tracked::{self, TrackedImage};
use super::util;
use crate::log::{c_string, format_text, log_info, log_warn};
use crate::runtime::gpu::registers::{
    CONSTANT_BASE_ALU, CONSTANT_BASE_FETCH, ColorInfo, ConstantWindow, DepthInfo, RB_COLOR_INFO, RB_COLORCONTROL,
    RB_DEPTH_INFO, RB_SURFACE_INFO, RegisterFile, SQ_PS_CONST, SQ_VS_CONST, SurfaceInfo,
};
use crate::runtime::gpu::texture::TextureFetch;
use ash::vk;
use std::collections::VecDeque;
use std::ffi::{CStr, CString, c_char, c_int};
use std::io::Write;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use super::texture_cache::TextureCache;

// ---- the switches, each read once ------------------------------------------------------------

fn text(name: &CStr) -> Option<&'static CStr> {
    if cfg!(feature = "diagnostics") { crate::env::text(name) } else { None }
}

fn number(name: &CStr, fallback: u64) -> u64 {
    if cfg!(feature = "diagnostics") { crate::env::number(name, fallback) } else { fallback }
}

fn flag(name: &CStr) -> bool {
    cfg!(feature = "diagnostics") && crate::env::flag(name)
}

fn real(name: &CStr, fallback: f64) -> f64 {
    if cfg!(feature = "diagnostics") { crate::env::real(name, fallback) } else { fallback }
}

macro_rules! once {
    ($name:ident: $type:ty = $value:expr) => {
        pub fn $name() -> $type {
            static VALUE: OnceLock<$type> = OnceLock::new();
            *VALUE.get_or_init(|| $value)
        }
    };
}

// Which target each pass writes, and under what state.
once!(trace_passes: u64 = number(c"MW2_TRACE_PASSES", 0));
// The frame the traces start at.
once!(trace_after: u64 = number(c"MW2_TRACE_RENDER_AFTER", 0));
once!(frame_dump_directory: Option<&'static CStr> = text(c"MW2_DUMP_FRAMES"));
once!(frame_dump_every: u64 = number(c"MW2_DUMP_FRAME_EVERY", 600).max(1));
once!(flash_frames_kept: u32 = number(c"MW2_FLASH_FRAMES", 0) as u32);
once!(flash_directory: &'static CStr = text(c"MW2_FLASH_DIR").unwrap_or(c"diagnosis/flashes"));
once!(recording_draws: bool =
    flash_frames_kept() > 0 && (flag(c"MW2_FLASH_HUNT") || flag(c"MW2_FLASH_DRAWS")));
once!(hash_frames: bool = flag(c"MW2_HASH_FRAMES"));
once!(dump_world_frames: bool = flag(c"MW2_DUMP_WORLD_FRAMES"));
once!(dump_targets_too: bool = flag(c"MW2_DUMP_TARGETS"));
once!(flash_mark_at: f64 = real(c"MW2_FLASH_MARK_AT", 0.0));
once!(no_depth_test: bool = flag(c"MW2_NO_DEPTH_TEST"));

/// Frame numbers are not comparable between runs; a dump taken at a fixed second of the
/// same scripted run is the same moment.
fn frame_dump_due() -> bool {
    static AFTER: OnceLock<f64> = OnceLock::new();
    let after = *AFTER.get_or_init(|| real(c"MW2_DUMP_FRAMES_AFTER_SECONDS", 0.0));
    if after <= 0.0 {
        return true;
    }
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64() >= after
}

/// Where a trace should fire: standing where MW2_WALK_TO asked, or past the frame
/// MW2_TRACE_RENDER_AFTER names.
pub fn diagnose_here(frames: u64) -> bool {
    #[cfg(feature = "diagnostics")]
    let arrived = crate::runtime::player::walking() && crate::runtime::player::arrived();
    #[cfg(not(feature = "diagnostics"))]
    let arrived = false;
    arrived || (trace_after() != 0 && frames >= trace_after())
}

unsafe extern "C" {
    fn strtoull(text: *const c_char, end: *mut *mut c_char, base: c_int) -> u64;
}

/// MW2_ONLY_SHADER=<hex>[,<hex>] keeps only draws whose vertex or pixel shader is named;
/// MW2_SKIP_SHADER drops them.
pub struct ShaderList {
    pub hashes: Vec<u64>,
}

impl ShaderList {
    fn new(variable: &CStr) -> ShaderList {
        let mut hashes = Vec::new();
        if let Some(value) = text(variable) {
            let mut at = value.as_ptr();
            // SAFETY: a C string; strtoull stops at its zero at the latest, and `end`
            // points into it.
            unsafe {
                while *at != 0 {
                    let mut end: *mut c_char = std::ptr::null_mut();
                    let hash = strtoull(at, &mut end, 16);
                    if std::ptr::eq(end, at) {
                        break;
                    }
                    hashes.push(hash);
                    at = if *end == b',' as c_char { end.add(1) } else { end };
                }
            }
        }
        ShaderList { hashes }
    }

    #[inline]
    pub fn names(&self, vertex_hash: u64, pixel_hash: u64) -> bool {
        self.hashes.iter().any(|&hash| hash == vertex_hash || hash == pixel_hash)
    }
}

pub fn only_shaders() -> &'static ShaderList {
    static LIST: OnceLock<ShaderList> = OnceLock::new();
    LIST.get_or_init(|| ShaderList::new(c"MW2_ONLY_SHADER"))
}

pub fn skip_shaders() -> &'static ShaderList {
    static LIST: OnceLock<ShaderList> = OnceLock::new();
    LIST.get_or_init(|| ShaderList::new(c"MW2_SKIP_SHADER"))
}

pub fn traced_shaders() -> &'static ShaderList {
    static LIST: OnceLock<ShaderList> = OnceLock::new();
    LIST.get_or_init(|| ShaderList::new(c"MW2_TRACE_SHADER_VERTS"))
}

/// What reads the finished frame back, decided before it is submitted, because a frame
/// read back has to be waited for.
#[derive(Clone, Copy, Default, Debug)]
pub struct FrameReadBacks {
    pub dump: bool,
    pub keep: bool,
    pub hash: bool, // MW2_HASH_FRAMES
}

impl FrameReadBacks {
    #[inline]
    pub fn any(&self) -> bool {
        self.dump || self.keep || self.hash
    }
}

// ---- the flash hunt's draw list --------------------------------------------------------------

/// Every draw of the last frames a flash hunt keeps, so a mark says which draw differed in
/// the frame that flashed.
#[derive(Clone, Copy, Default)]
pub struct DrawRecord {
    pub vertex_shader: u64,
    pub pixel_shader: u64,
    pub colour_tile: u32,
    pub depth_tile: u32,
    pub pitch: u32,
    pub samples: u32,
    pub primitive: u32,
    pub count: u32,
    pub vertex_address: u32,
    pub index_address: u32,
    pub depth_control: u32,
    pub blend: u32,
    pub colour_mask: u32,
    pub mode_cntl: u32,
    pub colour_control: u32,
    pub vertex_constants: u64,
    pub pixel_constants: u64,
    pub fetch_constants: u64,
    pub vertex_data: u64,
    pub indices: u64,
    pub texture_first: u32,
    pub texture_count: u32,
    pub skipped: Option<&'static CStr>,
}

#[derive(Clone, Copy, Default)]
pub struct DrawTexture {
    pub slot: u32,
    pub address: u32,
    pub format: u32,
    pub width: u32,
    pub height: u32,
    pub id: u64,
    pub version: u32, // times its image was staged; 0 for a resolve's
    pub resolved: bool,
}

#[derive(Default)]
struct DrawFrame {
    frame: u64,
    world: bool,
    draws: Vec<DrawRecord>,
    textures: Vec<DrawTexture>,
    late_restages: Vec<u64>,
}

struct Kept {
    pixels: Vec<u8>,
    frame: u64,
    texture_frame: u64,
}

/// F7, or Y on the pad: counted here, any thread.
static FLASHES_SEEN: AtomicU32 = AtomicU32::new(0);

/// The diagnostics' state: the read-back buffer, the flash recorder and its draw list.
pub struct Diagnostics {
    pub read_back: vk::Buffer,
    pub read_back_memory: vk::DeviceMemory,
    pub read_back_mapped: *const u8,
    // The flash recorder.
    ring: Vec<Kept>,
    next: usize,
    width: u32,
    height: u32,
    marks: u32,
    // The draw log, and the draw being recorded.
    open: DrawFrame,
    done: VecDeque<DrawFrame>,
    last_vertex_data: u64,
    last_vertex_address: u32,
    pub record: Option<usize>,
    pub pass_frames: u64,
    pub target_summary: Vec<CString>,
    // MW2_FLASH_MARK_AT's clock and whether it has marked.
    started: Option<Instant>,
    marked: bool,
}

impl Default for Diagnostics {
    fn default() -> Self {
        Diagnostics::new()
    }
}

// Eight bytes at a time; a fingerprint, not a checksum anyone relies on.
fn mix(mut hash: u64, bytes: &[u8]) -> u64 {
    hash ^= (bytes.len() as u64).wrapping_mul(0x9E3779B97F4A7C15);
    let mut words = bytes.chunks_exact(8);
    for word in &mut words {
        let word = u64::from_ne_bytes([word[0], word[1], word[2], word[3], word[4], word[5], word[6], word[7]]);
        hash = (hash ^ word).wrapping_mul(0xFF51AFD7ED558CCD);
        hash ^= hash >> 32;
    }
    for &byte in words.remainder() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001B3);
    }
    hash
}

// A long window by its ends and its length.
fn sample(hash: u64, bytes: &[u8]) -> u64 {
    const END: usize = 256;
    let size = bytes.len();
    if size <= 2 * END {
        return mix(hash, bytes);
    }
    let hash = mix(hash, &bytes[..END]);
    let hash = mix(hash, &bytes[size / 2 - END / 2..size / 2 - END / 2 + END]);
    mix(hash, &bytes[size - END..]) ^ size as u64
}

/// The register file's dwords [first, first + count) as the bytes the hash reads.
fn register_bytes(r: &RegisterFile, first: u32, count: usize) -> &[u8] {
    let words = &r.values[first as usize..first as usize + count];
    // SAFETY: u32s are plain bytes; the slice is inside the file.
    unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), words.len() * 4) }
}

fn window_hash(r: &RegisterFile, window_register: u32, needed: u32) -> u64 {
    let base = ConstantWindow(r.get(window_register)).base().min(256);
    let first = CONSTANT_BASE_ALU + base * 4;
    mix(u64::from(base), register_bytes(r, first, needed.min(512 - base) as usize * 4))
}

/// Guest bytes a note was handed: `size` at `bytes`, null for none.
fn guest_bytes<'a>(bytes: *const u8, size: usize) -> &'a [u8] {
    if bytes.is_null() || size == 0 {
        return &[];
    }
    // SAFETY: what the arena copied from, inside the guest's reservation, read while the
    // draw is made (the arena's contract for its notes).
    unsafe { std::slice::from_raw_parts(bytes, size) }
}

impl Diagnostics {
    pub fn new() -> Diagnostics {
        Diagnostics {
            read_back: vk::Buffer::null(),
            read_back_memory: vk::DeviceMemory::null(),
            read_back_mapped: std::ptr::null(),
            ring: Vec::new(),
            next: 0,
            width: 0,
            height: 0,
            marks: 0,
            open: DrawFrame::default(),
            done: VecDeque::new(),
            last_vertex_data: 0,
            last_vertex_address: 0,
            record: None,
            pass_frames: 0,
            target_summary: Vec::new(),
            started: None,
            marked: false,
        }
    }

    pub fn plan_read_backs(&self, frames: u64) -> FrameReadBacks {
        let mut plan = FrameReadBacks::default();
        if !cfg!(feature = "diagnostics") {
            return plan;
        }
        plan.dump = frame_dump_directory().is_some()
            && frames.wrapping_add(1).is_multiple_of(frame_dump_every())
            && frame_dump_due();
        plan.keep = flash_frames_kept() > 0;
        plan.hash = hash_frames();
        plan
    }

    /// Skip's half: the draw being recorded says why it was not drawn, the first reason.
    #[inline]
    pub fn skipped(&mut self, why: &'static CStr) {
        if let Some(at) = self.record {
            let record = &mut self.open.draws[at];
            if record.skipped.is_none() {
                record.skipped = Some(why);
            }
        }
    }

    /// The draw about to be made, recorded while a flash hunt keeps frames; otherwise
    /// nothing.
    pub fn begin_draw_record(
        &mut self,
        r: &RegisterFile,
        vertex_shader: u64,
        pixel_shader: u64,
        primitive: u32,
        count: u32,
        index_address: u32,
    ) {
        if !cfg!(feature = "diagnostics") || !recording_draws() {
            self.record = None;
            return;
        }
        let surface = SurfaceInfo(r.get(RB_SURFACE_INFO));
        let d = DrawRecord {
            vertex_shader,
            pixel_shader,
            primitive,
            count,
            index_address,
            colour_tile: ColorInfo(r.get(RB_COLOR_INFO)).base_tile(),
            depth_tile: DepthInfo(r.get(RB_DEPTH_INFO)).base_tile(),
            pitch: surface.pitch(),
            samples: surface.msaa_samples(),
            depth_control: r.get(RB_DEPTHCONTROL),
            blend: r.get(RB_BLENDCONTROL0),
            colour_mask: r.get(RB_COLOR_MASK),
            mode_cntl: r.get(PA_SU_SC_MODE_CNTL),
            colour_control: r.get(RB_COLORCONTROL),
            fetch_constants: mix(0, register_bytes(r, CONSTANT_BASE_FETCH, 32 * 6)),
            texture_first: self.open.textures.len() as u32,
            ..DrawRecord::default()
        };
        self.open.draws.push(d);
        self.record = Some(self.open.draws.len() - 1);
    }

    /// A texture the draw being recorded binds. Its version is the
    /// cache's, filled in by draw_texture_versions once the lookup is done.
    pub fn note_draw_texture(&mut self, slot: u32, fetch: &TextureFetch, id: u64, resolved: bool) {
        let Some(at) = self.record else { return };
        self.open.textures.push(DrawTexture {
            slot,
            address: fetch.base_address(),
            format: fetch.format(),
            width: fetch.width(),
            height: fetch.height(),
            id,
            version: 0,
            resolved,
        });
        self.open.draws[at].texture_count += 1;
    }

    /// The versions of the textures the draw being recorded bound: the cache's version of
    /// each that is not a resolve's.
    pub fn draw_texture_versions(&mut self, textures: &TextureCache) {
        let Some(at) = self.record else { return };
        let d = self.open.draws[at];
        let range = d.texture_first as usize..(d.texture_first + d.texture_count) as usize;
        for texture in &mut self.open.textures[range] {
            if !texture.resolved {
                texture.version = textures.version(texture.id);
            }
        }
    }

    /// The frame whose picture carries this renderer frame number is done.
    pub fn end_frame_draws(&mut self, textures: &mut TextureCache, frame: u64, world_drawn: bool) {
        // The frame's late re-stages are taken every frame, kept or not.
        if !cfg!(feature = "diagnostics") || !recording_draws() {
            let mut unkept = Vec::new();
            textures.take_late_restages(&mut unkept);
            return;
        }
        let mut f = std::mem::take(&mut self.open);
        f.frame = frame;
        f.world = world_drawn;
        textures.take_late_restages(&mut f.late_restages);
        self.done.push_back(f);
        while self.done.len() > flash_frames_kept() as usize + 4 {
            self.done.pop_front();
        }
    }

    fn keep_flash_frame(&mut self, textures: &TextureCache, rgba: Vec<u8>, width: u32, height: u32, frame: u64) {
        if rgba.len() < width as usize * height as usize * 4 {
            return;
        }
        if width != self.width || height != self.height {
            self.ring.clear();
            self.next = 0;
            self.width = width;
            self.height = height;
        }
        if self.ring.len() < flash_frames_kept() as usize {
            self.ring.push(Kept { pixels: Vec::new(), frame: 0, texture_frame: 0 });
        }
        let kept = &mut self.ring[self.next];
        self.next = (self.next + 1) % flash_frames_kept() as usize;
        kept.pixels = rgba;
        kept.frame = frame;
        kept.texture_frame = textures.frame_number();
    }

    /// On the render thread, the frame after the key: the frames kept before
    /// it, and their draw lists, written off this thread.
    fn write_flashes(&mut self, textures: &TextureCache, frames: u64) {
        let seen = FLASHES_SEEN.load(Ordering::Relaxed);
        if seen == self.marks {
            return;
        }
        self.marks = seen;
        #[cfg(feature = "diagnostics")]
        let here = crate::runtime::player::locate().map(|w| (w.x, w.y, w.z, w.yaw));
        #[cfg(not(feature = "diagnostics"))]
        let here: Option<(f32, f32, f32, f32)> = None;
        let standing = match here {
            Some((x, y, z, yaw)) => format_text!(
                ", standing at %.1f %.1f %.1f yaw %.1f",
                f64::from(x),
                f64::from(y),
                f64::from(z),
                f64::from(yaw)
            ),
            None => CString::default(),
        };
        log_warn!(
            "FLASH MARK %u: now at renderer frame %llu, texture frame %llu%s",
            seen,
            frames,
            textures.frame_number(),
            standing.as_c_str()
        );
        if self.ring.is_empty() {
            return;
        }
        // Oldest first: a second of frames is a few hundred megabytes and the game should
        // not stop for it.
        let count = self.ring.len();
        let mut ring = std::mem::take(&mut self.ring);
        let mut kept: Vec<Kept> = Vec::with_capacity(count);
        let first = self.next % count;
        kept.extend(ring.drain(first..));
        kept.append(&mut ring);
        self.next = 0;
        let dir = format_text!("%s/mark_%02u", flash_directory(), seen);
        let _ = std::fs::create_dir_all(os_path(dir.as_bytes()));
        log_warn!(
            "FLASH MARK %u: writing %llu frames, renderer %llu..%llu, texture %llu..%llu, to %s",
            seen,
            kept.len() as u64,
            kept[0].frame,
            kept[count - 1].frame,
            kept[0].texture_frame,
            kept[count - 1].texture_frame,
            dir.as_c_str()
        );
        // The draw lists of the same frames, taken now: the ring moves on.
        let (low, high) = (kept[0].frame, kept[count - 1].frame);
        let draws: Vec<DrawFrame> = self.done.drain(..).filter(|d| d.frame >= low && d.frame <= high).collect();
        let (width, height) = (self.width, self.height);
        let path = dir.into_bytes();
        // Detached: the platform's default stack, no name.
        let _ =
            std::thread::Builder::new().stack_size(crate::runtime::platform::default_thread_stack()).spawn(move || {
                for d in &draws {
                    write_draw_frame(&path, d);
                }
                for k in &kept {
                    let name = format!("r{:06}_t{:06}", k.frame, k.texture_frame);
                    write_pnm(&path, &name, &k.pixels, width, 0, 0, width, height);
                }
            });
    }

    /// Kept as text because the report is written after the images are gone.
    pub fn summarise_targets(&mut self, targets: &Targets, frames: u64) {
        self.target_summary.clear();
        for (key, target) in targets.iter() {
            if !target.depth && target.draws == 0 && target.presented == 0 {
                continue;
            }
            let arguments = (
                target.width,
                target.height,
                key.base_tile,
                key.pitch,
                target.guest_format,
                1u32.wrapping_shl(key.samples),
                target.samples.as_raw(),
                target.draws,
                target.frames_with_colour,
                frames,
                target.presented,
            );
            let line = if target.depth {
                format_text!(
                    "%ux%u DEPTH at tile %u, pitch %u, format %u, %ux MSAA drawn %ux: %llu draws in %llu of %llu frames, presented %llu times",
                    arguments.0,
                    arguments.1,
                    arguments.2,
                    arguments.3,
                    arguments.4,
                    arguments.5,
                    arguments.6,
                    arguments.7,
                    arguments.8,
                    arguments.9,
                    arguments.10
                )
            } else {
                format_text!(
                    "%ux%u colour at tile %u, pitch %u, format %u, %ux MSAA drawn %ux: %llu draws in %llu of %llu frames, presented %llu times",
                    arguments.0,
                    arguments.1,
                    arguments.2,
                    arguments.3,
                    arguments.4,
                    arguments.5,
                    arguments.6,
                    arguments.7,
                    arguments.8,
                    arguments.9,
                    arguments.10
                )
            };
            // At most 223 bytes a line.
            self.target_summary.push(c_string(&line.as_bytes()[..line.as_bytes().len().min(223)]));
        }
    }
}

impl DrawNotes for Diagnostics {
    fn constant_windows(&mut self, r: &RegisterFile, vertex_needed: u32, pixel_needed: u32) {
        let Some(at) = self.record else { return };
        let d = &mut self.open.draws[at];
        d.vertex_constants = window_hash(r, SQ_VS_CONST, vertex_needed);
        d.pixel_constants = window_hash(r, SQ_PS_CONST, pixel_needed);
    }

    fn vertex_window(&mut self, bytes: *const u8, size: usize) {
        let Some(at) = self.record else { return };
        let d = &mut self.open.draws[at];
        if bytes.is_null() {
            d.vertex_data = self.last_vertex_data;
            d.vertex_address = self.last_vertex_address;
            return;
        }
        d.vertex_data = sample(d.vertex_data, guest_bytes(bytes, size));
        self.last_vertex_data = d.vertex_data;
        self.last_vertex_address = d.vertex_address;
    }

    fn vertex_buffer(&mut self, physical: u32) {
        if let Some(at) = self.record {
            let d = &mut self.open.draws[at];
            if d.vertex_address == 0 {
                d.vertex_address = physical;
            }
        }
    }

    fn indices(&mut self, bytes: *const u8, size: usize) {
        if let Some(at) = self.record {
            self.open.draws[at].indices = sample(0, guest_bytes(bytes, size));
        }
    }
}

#[cfg(unix)]
fn os_path(bytes: &[u8]) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::OsStr::from_bytes(bytes).into()
}

#[cfg(not(unix))]
fn os_path(bytes: &[u8]) -> std::path::PathBuf {
    String::from_utf8_lossy(bytes).into_owned().into()
}

fn write_draw_frame(dir: &[u8], f: &DrawFrame) {
    let mut name = dir.to_vec();
    name.extend_from_slice(format!("/draws_r{:06}.txt", f.frame).as_bytes());
    let Ok(file) = std::fs::File::create(os_path(&name)) else { return };
    let mut out = std::io::BufWriter::new(file);
    let _ = writeln!(out, "frame {} world {} draws {}", f.frame, f.world as i32, f.draws.len());
    let _ = write!(out, "late-restaged");
    for id in &f.late_restages {
        let _ = write!(out, " {id:x}");
    }
    let _ = writeln!(out);
    for (i, d) in f.draws.iter().enumerate() {
        let _ = write!(
            out,
            "{i} vs={:016x} ps={:016x} tgt={}/{}/{}/{} prim={} n={} va={:08x} ia={:08x} dc={:08x} bl={:08x} cm={:08x} mc={:08x} cc={:08x} vc={:016x} pc={:016x} fc={:016x} vd={:016x} ix={:016x}",
            d.vertex_shader,
            d.pixel_shader,
            d.colour_tile,
            d.pitch,
            d.samples,
            d.depth_tile,
            d.primitive,
            d.count,
            d.vertex_address,
            d.index_address,
            d.depth_control,
            d.blend,
            d.colour_mask,
            d.mode_cntl,
            d.colour_control,
            d.vertex_constants,
            d.pixel_constants,
            d.fetch_constants,
            d.vertex_data,
            d.indices
        );
        for t in 0..d.texture_count {
            let x = &f.textures[(d.texture_first + t) as usize];
            let _ = write!(
                out,
                " t{}={}:{:08x}:{}:{}x{}:{:x}:v{}",
                x.slot,
                if x.resolved { 'R' } else { 'T' },
                x.address,
                x.format,
                x.width,
                x.height,
                x.id,
                x.version
            );
        }
        if let Some(why) = d.skipped {
            let _ = write!(out, " SKIPPED=\"{}\"", why.to_string_lossy());
        }
        let _ = writeln!(out);
    }
}

/// The rectangle x0,y0 to x1,y1 of an RGBA image `width` wide, as a PNM.
#[allow(clippy::too_many_arguments)]
fn write_pnm(dir: &[u8], name: &str, rgba: &[u8], width: u32, x0: u32, y0: u32, x1: u32, y1: u32) {
    let mut path = dir.to_vec();
    path.extend_from_slice(format!("/{name}.pnm").as_bytes());
    let Ok(file) = std::fs::File::create(os_path(&path)) else { return };
    let mut out = std::io::BufWriter::new(file);
    let _ = write!(out, "P6\n{} {}\n255\n", x1.wrapping_sub(x0), y1.wrapping_sub(y0));
    for y in y0..y1 {
        for x in x0..x1 {
            let at = (y as usize * width as usize + x as usize) * 4;
            let _ = out.write_all(&rgba[at..at + 3]);
        }
    }
}

/// F7, or Y on the pad during a flash hunt: somebody saw a flash. Logged at once, any
/// thread: a count and a line.
pub fn flash_seen(how: &CStr) {
    let n = FLASHES_SEEN.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    log_warn!("FLASH MARK %u: %s pressed", n, how);
}

/// The bytes a texel of `format` takes when copied out: eight for the 64-bit colour
/// formats (16_16_16_16, 16_16_16_16_FLOAT and 32_32_FLOAT surfaces), four for the rest
/// and for a depth aspect, so that a dump's copy fits its buffer.
pub fn dump_texel_bytes(format: vk::Format, aspect: vk::ImageAspectFlags) -> u32 {
    if aspect != vk::ImageAspectFlags::COLOR {
        return 4;
    }
    match format {
        vk::Format::R16G16B16A16_SNORM | vk::Format::R16G16B16A16_SFLOAT | vk::Format::R32G32_SFLOAT => 8,
        _ => 4,
    }
}

fn half(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0f32 } else { 1.0 };
    let exponent = u32::from((bits >> 10) & 0x1F);
    let mantissa = f32::from(bits & 0x3FF);
    match exponent {
        0 => sign * mantissa * (1.0 / 16_777_216.0),
        31 => {
            if mantissa == 0.0 {
                sign * f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => sign * (1.0 + mantissa / 1024.0) * f32::powi(2.0, exponent as i32 - 15),
    }
}

fn eight(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0) as u8
}

/// A 64-bit texel's channels as eight bits each, for a picture to look at.
fn as_rgba8(texels: &[u8], format: vk::Format, pixels: usize) -> Vec<u8> {
    let mut out = vec![255u8; pixels * 4];
    for (i, texel) in texels.chunks_exact(8).take(pixels).enumerate() {
        let channel = |n: usize| [texel[n * 2], texel[n * 2 + 1]];
        let rgba = &mut out[i * 4..i * 4 + 4];
        match format {
            vk::Format::R16G16B16A16_SNORM => {
                for (n, byte) in rgba.iter_mut().enumerate() {
                    *byte = eight(f32::from(i16::from_le_bytes(channel(n)).max(-32767)) / 32767.0);
                }
            }
            vk::Format::R16G16B16A16_SFLOAT => {
                for (n, byte) in rgba.iter_mut().enumerate() {
                    *byte = eight(half(u16::from_le_bytes(channel(n))));
                }
            }
            _ => {
                let red = f32::from_le_bytes([texel[0], texel[1], texel[2], texel[3]]);
                let green = f32::from_le_bytes([texel[4], texel[5], texel[6], texel[7]]);
                rgba[0] = eight(red);
                rgba[1] = eight(green);
                rgba[2] = 0;
            }
        }
    }
    out
}

/// RGBA pixels as a PNM in MW2_DUMP_FRAMES, saying how much of it is not black.
fn write_dump(name: &[u8], rgba: &[u8], width: u32, height: u32) {
    let Some(dir) = frame_dump_directory() else { return };
    let mut path = dir.to_bytes().to_vec();
    path.push(b'/');
    path.extend_from_slice(name);
    path.extend_from_slice(b".pnm");
    path.truncate(511);
    let Ok(file) = std::fs::File::create(os_path(&path)) else { return };
    let mut out = std::io::BufWriter::new(file);
    let _ = write!(out, "P6\n{width} {height}\n255\n");
    let pixels = width as usize * height as usize;
    let mut lit = 0usize;
    for pixel in rgba.chunks_exact(4).take(pixels) {
        let _ = out.write_all(&pixel[..3]);
        lit += usize::from(pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 0);
    }
    drop(out);
    let path = c_string(path);
    log_info!(
        "renderer: wrote %s (%llu%% of pixels not black)",
        path.as_c_str(),
        (lit * 100).checked_div(pixels).unwrap_or(0) as u64
    );
}

/// Depth spends its whole range near one, so a linear ramp is a white page: the picture is
/// what the image holds, stretched over the range it uses.
fn depth_as_grey(texels: &[u8], width: u32, height: u32, format: vk::Format, name: &CStr) -> Vec<u8> {
    let pixels = width as usize * height as usize;
    let packed = format == vk::Format::D24_UNORM_S8_UINT || format == vk::Format::X8_D24_UNORM_PACK32;
    let mut depth = vec![0f32; pixels];
    let (mut lo, mut hi) = (1e30f32, -1e30f32);
    for (i, value) in depth.iter_mut().enumerate() {
        let bytes = [texels[i * 4], texels[i * 4 + 1], texels[i * 4 + 2], texels[i * 4 + 3]];
        *value = if packed {
            (u32::from_le_bytes(bytes) & 0xFFFFFF) as f32 / 0xFFFFFF as f32
        } else {
            f32::from_le_bytes(bytes)
        };
        // std::min and std::max: the first unless the second is less (greater).
        if *value < lo {
            lo = *value;
        }
        if hi < *value {
            hi = *value;
        }
    }
    log_info!("renderer: %s depth spans %.6f..%.6f", name, f64::from(lo), f64::from(hi));
    let span = if hi > lo { hi - lo } else { 1.0 };
    let mut grey = vec![255u8; pixels * 4];
    for (i, &value) in depth.iter().enumerate() {
        let v = (((value - lo) / span).clamp(0.0, 1.0) * 255.0) as u8;
        grey[i * 4] = v;
        grey[i * 4 + 1] = v;
        grey[i * 4 + 2] = v;
    }
    grey
}

impl Core {
    /// The buffer frames are read back into, made on first use.
    fn make_read_back(&mut self, vk: &Vk) -> bool {
        let d = &mut self.diag;
        if d.read_back != vk::Buffer::null() {
            return true;
        }
        let bytes = u64::from(self.present_width) * u64::from(self.present_height) * 4;
        let info =
            vk::BufferCreateInfo { size: bytes, usage: vk::BufferUsageFlags::TRANSFER_DST, ..Default::default() };
        // SAFETY (each call): the renderer's device and what it made here.
        unsafe {
            if vk.create_buffer(&info, &mut d.read_back) != vk::Result::SUCCESS {
                return false;
            }
            let needs = vk.get_buffer_memory_requirements(d.read_back);
            // Cached if the device has it: the host reads every byte of it.
            let visible = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
            let mut kind =
                util::find_memory(&self.memory, needs.memory_type_bits, visible | vk::MemoryPropertyFlags::HOST_CACHED);
            if kind == u32::MAX {
                kind = util::find_memory(&self.memory, needs.memory_type_bits, visible);
            }
            let allocate =
                vk::MemoryAllocateInfo { allocation_size: needs.size, memory_type_index: kind, ..Default::default() };
            let mut mapped: *mut std::ffi::c_void = std::ptr::null_mut();
            if kind == u32::MAX
                || vk.allocate_memory(&allocate, &mut d.read_back_memory) != vk::Result::SUCCESS
                || vk.bind_buffer_memory(d.read_back, d.read_back_memory, 0) != vk::Result::SUCCESS
                || vk.map_memory(d.read_back_memory, 0, bytes, &mut mapped) != vk::Result::SUCCESS
            {
                log_warn!("renderer: no buffer to read frames back into");
                return false;
            }
            d.read_back_mapped = mapped.cast_const().cast();
        }
        true
    }

    /// Copies the finished frame out, inside its own command buffer.
    pub fn record_read_back(&mut self, vk: &Vk, present: vk::Image) {
        if !self.make_read_back(vk) {
            return;
        }
        let region = vk::BufferImageCopy {
            image_subresource: vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            },
            image_extent: vk::Extent3D { width: self.present_width, height: self.present_height, depth: 1 },
            ..Default::default()
        };
        let buffer = self.diag.read_back;
        recorder::command(move |c| {
            let to_host = vk::BufferMemoryBarrier {
                src_access_mask: vk::AccessFlags::TRANSFER_WRITE,
                dst_access_mask: vk::AccessFlags::HOST_READ,
                src_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                dst_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                buffer,
                size: vk::WHOLE_SIZE,
                ..Default::default()
            };
            // SAFETY: the recorder's command buffer; the present image is in TRANSFER_SRC
            // (the swap left it so), the buffer the renderer's.
            unsafe {
                c.vk.cmd_copy_image_to_buffer(
                    c.into,
                    present,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    buffer,
                    &[region],
                );
                c.vk.cmd_pipeline_barrier(
                    c.into,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::HOST,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[to_host],
                    &[],
                );
            }
        });
    }

    /// Copies an image out and writes it, waiting for the copy, for the target dumps.
    /// `track` is what the image was last used for; it is left in the copy's
    /// use, or brought back to `home` where its users cannot bring it on themselves
    /// (tracked.rs). `aspect` says which to read, `format` what it holds.
    #[allow(clippy::too_many_arguments)]
    fn dump_image(
        &mut self,
        vk: &Vk,
        image: vk::Image,
        track: &mut TrackedImage,
        home: Option<TrackedImage>,
        width: u32,
        height: u32,
        name: &[u8],
        aspect: vk::ImageAspectFlags,
        format: vk::Format,
    ) {
        let texel = dump_texel_bytes(format, aspect);
        let bytes = width.wrapping_mul(height).wrapping_mul(texel);
        let mut buffer = vk::Buffer::null();
        let mut memory = vk::DeviceMemory::null();
        let info = vk::BufferCreateInfo {
            size: u64::from(bytes),
            usage: vk::BufferUsageFlags::TRANSFER_DST,
            ..Default::default()
        };
        // SAFETY (each call): the renderer's device and its set-up command buffer and
        // fence, which nothing else records into while this waits.
        unsafe {
            if vk.create_buffer(&info, &mut buffer) != vk::Result::SUCCESS {
                return;
            }
            let needs = vk.get_buffer_memory_requirements(buffer);
            let kind = util::find_memory(
                &self.memory,
                needs.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            );
            let allocate =
                vk::MemoryAllocateInfo { allocation_size: needs.size, memory_type_index: kind, ..Default::default() };
            if kind == u32::MAX || vk.allocate_memory(&allocate, &mut memory) != vk::Result::SUCCESS {
                vk.destroy_buffer(buffer);
                return;
            }
            let _ = vk.bind_buffer_memory(buffer, memory, 0);

            // Submitted from here, so after what the recorder thread still holds.
            super::renderer_frame::drain_recorder();
            let begin = vk::CommandBufferBeginInfo {
                flags: vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT,
                ..Default::default()
            };
            let _ = vk.begin_command_buffer(self.setup, &begin);
            // After its last use.
            // Depth and stencil change layout together; only the depth is read.
            let all = if aspect == vk::ImageAspectFlags::DEPTH { DEPTH_ASPECTS } else { aspect };
            if let Some(t) = track.enter(tracked::COPIED_FROM, true) {
                util::renderer_transition(vk, self.setup, image, all, &t);
            }
            let region = vk::BufferImageCopy {
                image_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: aspect,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                },
                image_extent: vk::Extent3D { width, height, depth: 1 },
                ..Default::default()
            };
            vk.cmd_copy_image_to_buffer(self.setup, image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, buffer, &[region]);
            if let Some(t) = home.and_then(|home| track.enter(home, true)) {
                util::renderer_transition(vk, self.setup, image, all, &t);
            }
            let _ = vk.end_command_buffer(self.setup);
            let commands = [self.setup];
            let submit =
                vk::SubmitInfo { command_buffer_count: 1, p_command_buffers: commands.as_ptr(), ..Default::default() };
            {
                let _lock = super::QueueLock::hold();
                let _ = vk.reset_fences(&[self.setup_fence]);
                let _ = vk.queue_submit(self.queue, &[submit], self.setup_fence);
            }
            super::failed(vk.wait_for_fences(&[self.setup_fence], true, u64::MAX), c"the set-up fence");

            let mut mapped: *mut std::ffi::c_void = std::ptr::null_mut();
            if vk.map_memory(memory, 0, u64::from(bytes), &mut mapped) == vk::Result::SUCCESS {
                let texels = std::slice::from_raw_parts(mapped.cast::<u8>().cast_const(), bytes as usize);
                let pixels = width as usize * height as usize;
                if aspect == vk::ImageAspectFlags::DEPTH {
                    let name_c = c_string(name);
                    write_dump(name, &depth_as_grey(texels, width, height, format, &name_c), width, height);
                } else if texel == 8 {
                    write_dump(name, &as_rgba8(texels, format, pixels), width, height);
                } else {
                    write_dump(name, texels, width, height);
                }
                vk.unmap_memory(memory);
            }
            vk.destroy_buffer(buffer);
            vk.free_memory(memory);
        }
    }

    /// MW2_DUMP_TARGETS: every colour target, depth target and resolve destination beside
    /// the frame.
    fn dump_targets(&mut self, vk: &Vk, targets: &mut Targets, records: &mut ResolveRecords) {
        let frames = self.frames;
        let name96 = |text: CString| -> Vec<u8> {
            let bytes = text.into_bytes();
            bytes[..bytes.len().min(95)].to_vec()
        };
        for (key, id) in targets.ids() {
            let target = *targets.get(id);
            if target.depth || target.draws == 0 || target.samples != vk::SampleCountFlags::TYPE_1 {
                continue;
            }
            let name = name96(format_text!(
                "frame_%06llu_tile%u_fmt%u_%ux%u_%ux",
                frames,
                key.base_tile,
                target.guest_format,
                target.width,
                target.height,
                1u32.wrapping_shl(key.samples)
            ));
            self.dump_image(
                vk,
                target.image,
                &mut targets.get_mut(id).track,
                None,
                target.width.wrapping_mul(target.scale),
                target.height.min(1024).wrapping_mul(target.scale),
                &name,
                vk::ImageAspectFlags::COLOR,
                target.format,
            );
        }

        // A multisampled image cannot be copied out, so it is resolved into its twin first,
        // and every depth image is made ready to be copied from: all of it recorded and
        // submitted before the copies read what it wrote.
        struct Shot {
            image: vk::Image,
            key: TargetKey,
            id: u32,
        }
        let mut shots = Vec::new();
        for (key, id) in targets.ids() {
            let target = *targets.get(id);
            if target.draws == 0 || (!target.depth && target.samples == vk::SampleCountFlags::TYPE_1) {
                continue;
            }
            if !self.begin_frame(vk) {
                break;
            }
            let tall = target.height.min(1024);
            let at = vk::Offset2D { x: 0, y: 0 };
            // SAFETY: the renderer's device; the target is live.
            let image = unsafe {
                if target.depth {
                    let mut f = FrameView { vk, core: self };
                    targets.begin_reading_depth(vk, &mut f, id, at, target.width, tall)
                } else {
                    targets.begin_reading_colour(vk, id, at, target.width, tall)
                }
            };
            if image != vk::Image::null() {
                shots.push(Shot { image, key, id });
            }
        }
        if !shots.is_empty() {
            self.submit(vk, true);
        }
        for shot in &shots {
            let target = *targets.get(shot.id);
            let tall = target.height.min(1024);
            let name = if target.depth {
                name96(format_text!(
                    "frame_%06llu_depth_tile%u_%ux%u_%ux",
                    frames,
                    shot.key.base_tile,
                    target.width,
                    tall,
                    1u32.wrapping_shl(shot.key.samples)
                ))
            } else {
                name96(format_text!(
                    "frame_%06llu_tile%u_fmt%u_%ux%u_%ux",
                    frames,
                    shot.key.base_tile,
                    target.guest_format,
                    target.width,
                    target.height,
                    1u32.wrapping_shl(shot.key.samples)
                ))
            };
            let held = targets.get_mut(shot.id);
            let track =
                if held.samples == vk::SampleCountFlags::TYPE_1 { &mut held.track } else { &mut held.single_track };
            self.dump_image(
                vk,
                shot.image,
                track,
                None,
                target.width.wrapping_mul(target.scale),
                tall.wrapping_mul(target.scale),
                &name,
                if target.depth { vk::ImageAspectFlags::DEPTH } else { vk::ImageAspectFlags::COLOR },
                target.format,
            );
        }

        // A frame begun after a depth shot; the target's next use brings it back into its
        // layout (tracked.rs).
        for shot in &shots {
            if targets.get(shot.id).depth {
                let _ = self.begin_frame(vk);
            }
        }

        // By address, so the order does not hang on the map's.
        let mut destinations: Vec<u32> = records.resolved_to.keys().copied().collect();
        destinations.sort_unstable();
        for address in destinations {
            let ring = records.resolved_to.get_mut(&address).expect("an address listed above");
            let newest = ring.newest as usize;
            let resolved = ring.copies[newest];
            if resolved.image == vk::Image::null() {
                continue;
            }
            let name = name96(format_text!(
                "frame_%06llu_resolve_%08X%s",
                frames,
                address,
                if resolved.depth { c"_depth" } else { c"" }
            ));
            // Sampled again afterwards: draws cannot bring it on themselves.
            self.dump_image(
                vk,
                resolved.image,
                &mut ring.copies[newest].track,
                Some(tracked::SAMPLED),
                resolved.width.wrapping_mul(resolved.scale),
                resolved.height.wrapping_mul(resolved.scale),
                &name,
                if resolved.depth { vk::ImageAspectFlags::DEPTH } else { vk::ImageAspectFlags::COLOR },
                resolved.format,
            );
        }
    }

    /// The dumps and the flash recorder, once the frame has been submitted and waited for.
    pub fn examine_frame(
        &mut self,
        vk: &Vk,
        world_drawn: bool,
        plan: FrameReadBacks,
        targets: &mut Targets,
        records: &mut ResolveRecords,
    ) {
        if !cfg!(feature = "diagnostics") {
            return;
        }
        if trace_passes() != 0 && diagnose_here(self.frames) {
            self.diag.pass_frames += 1;
        }
        let mut frame = Vec::new();
        if plan.any() && !self.diag.read_back_mapped.is_null() {
            let size = self.present_width as usize * self.present_height as usize * 4;
            // SAFETY: the read-back buffer's mapping, the frame's copy done (the frame was
            // submitted waiting, plan.any()).
            frame.extend_from_slice(unsafe { std::slice::from_raw_parts(self.diag.read_back_mapped, size) });
        }
        // MW2_HASH_FRAMES=1: the present image, as the window is handed it.
        #[cfg(feature = "diagnostics")]
        if plan.hash && !frame.is_empty() {
            log_info!("renderer: frame %llu hash %016llx", self.frames, super::trace::fnv64(&frame));
        }
        if plan.dump && !frame.is_empty() && (!dump_world_frames() || world_drawn) {
            let name = format!("frame_{:06}", self.frames);
            write_dump(name.as_bytes(), &frame, self.present_width, self.present_height);
            if dump_targets_too() {
                self.dump_targets(vk, targets, records);
            }
        }
        if plan.keep && !frame.is_empty() {
            let (width, height, frames) = (self.present_width, self.present_height, self.frames);
            self.diag.keep_flash_frame(&self.textures, frame, width, height, frames);
        }
        // MW2_FLASH_MARK_AT=<seconds>: a mark nobody pressed, to test the recorder in a run
        // with no one watching.
        let mark_at = flash_mark_at();
        let started = *self.diag.started.get_or_insert_with(Instant::now);
        if mark_at > 0.0 && !self.diag.marked && started.elapsed().as_secs_f64() >= mark_at {
            self.diag.marked = true;
            flash_seen(c"MW2_FLASH_MARK_AT");
        }
        let frames = self.frames;
        self.diag.write_flashes(&self.textures, frames);
    }
}

// ---- the report ----------------------------------------------------------------------------

/// What the report prints that the renderer held, taken at its shutdown: the
/// report is written after the shutdown, when the renderer is gone.
#[cfg_attr(not(feature = "diagnostics"), allow(dead_code, reason = "the report and the diagnostic keys' alone"))]
#[derive(Default)]
pub struct RendererReport {
    pub frames: u64,
    pub draws: u64,
    pub draws_recorded: u64,
    pub resolves: u64,
    pub depth_resolves: u64,
    pub depth_resolve_wraps: u64,
    pub depth_resolve_wraps_below: u64,
    pub resolve_clears: super::renderer_targets::ResolveClears,
    pub pipeline_count: u64,
    pub shader_count: u64,
    pub slots_kept: u64,
    pub slots_looked_up: u64,
    pub times: super::renderer_frame::Times,
    pub resolve_nanoseconds: u64,
    pub world_frames: u64,
    pub world_seconds: f64,
    pub submits: u64,
    pub segments: u64,
    pub stream_write_segments: u64,
    pub completions: u64,
    pub prewarm_wanted: u32,
    pub prewarmed: u32,
    pub prewarm_microseconds: u64,
    pub translate_microseconds: u64,
    pub compile_microseconds: u64,
    pub shaders_built_at_draw: u64,
    pub pipelines_linked: u64,
    pub pipelines_optimised: u64,
    pub target_summary: Vec<CString>,
    pub resolve_images_made: u64,
    pub arena_peak: u32,
    pub arena_bytes: u32,
    pub arena_written: [u64; ARENA_KINDS],
    pub arena_reused: [u64; ARENA_KINDS],
    pub verts_matched: u64,
    pub verts_traced: u64,
    pub arena_overflows: u64,
    /// By count, then by the reason's address, most first.
    pub skipped: Vec<(u64, &'static CStr)>,
    pub display_applied: u64,
    pub display_uploads: u64,
    pub display_refused: u64,
}

// SAFETY: plain counts and 'static texts.
unsafe impl Send for RendererReport {}

/// The renderer's report after its shutdown, moved here by Renderer::shutdown.
pub static REPORT: std::sync::Mutex<Option<RendererReport>> = std::sync::Mutex::new(None);

/// The renderer's report: from REPORT and the counters kept in statics. Without a shutdown
/// (the fault path), or with REPORT held, only the statics' lines.
#[cfg_attr(not(feature = "diagnostics"), allow(dead_code, reason = "the report and the diagnostic keys' alone"))]
pub fn report() {
    if !cfg!(feature = "diagnostics") {
        return;
    }
    #[cfg(feature = "diagnostics")]
    {
        let forced = super::trace::counts();
        let load = |counter: &std::sync::atomic::AtomicU64| counter.load(Ordering::Relaxed);
        if super::trace::refused_libraries() != 0 {
            log_info!(
                "renderer: MW2_FORCE_REFUSED_LIBRARIES: %llu libraries refused, %llu modules made again for a whole pipeline",
                load(&forced.refused_libraries),
                load(&forced.remade_modules)
            );
        }
    }
    report_capture();
    // Never waits: the fault path reaches here from a signal handler, perhaps on the thread
    // already inside this report or the shutdown moving it in, and a lock taken again there
    // would hang where the report should print and exit. Held, it reports as without a
    // shutdown.
    let held = match REPORT.try_lock() {
        Ok(held) => Some(held),
        Err(std::sync::TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    };
    let Some(r) = held.as_ref().and_then(|held| held.as_ref()) else {
        report_gathered();
        return;
    };
    if r.display_applied != 0 || r.display_uploads != 0 {
        log_info!(
            "display: the title's colour table applied to %llu frames (%llu table changes)",
            r.display_applied,
            r.display_uploads
        );
    }
    if r.display_refused != 0 {
        log_info!(
            "display: %llu frames went out without the table: their image was past the 8 that have a set",
            r.display_refused
        );
    }
    if r.draws == 0 && r.frames == 0 {
        return;
    }
    log_info!(
        "renderer: %llu frames, %llu of %llu draws recorded, %llu resolves (%llu of depth), %llu pipelines over %llu shaders",
        r.frames,
        r.draws_recorded,
        r.draws,
        r.resolves,
        r.depth_resolves,
        r.pipeline_count,
        r.shader_count
    );
    report_gathered();
    if r.slots_kept != 0 || r.slots_looked_up != 0 {
        log_info!(
            "renderer: texture slots bound: %llu kept from the draw before, %llu looked up",
            r.slots_kept,
            r.slots_looked_up
        );
    }
    let t = &r.times;
    if t.draw != 0 || t.submit != 0 {
        log_info!(
            "renderer: %llu ms in Draw (%llu ms of it binding textures), %llu ms in Resolve, %llu ms in Submit, %llu ms waiting for the queue",
            t.draw / 1000000,
            t.texture / 1000000,
            r.resolve_nanoseconds / 1000000,
            t.submit / 1000000,
            t.fence / 1000000
        );
    }
    if t.constants != 0 || t.record != 0 {
        log_info!(
            "renderer: of that, %llu ms on targets and passes, %llu ms on indices, %llu ms assembling constants, %llu ms on pipeline and texture state (%llu ms of it choosing pipelines), %llu ms recording commands",
            t.setup / 1000000,
            t.index / 1000000,
            t.constants / 1000000,
            t.state / 1000000,
            t.pipeline / 1000000,
            t.record / 1000000
        );
    }
    #[cfg(feature = "diagnostics")]
    if crate::runtime::stutters::on() {
        crate::runtime::stutters::report();
    }
    {
        let mut preparing = super::renderer_pipelines::Pipelines::new(false, vk::Format::UNDEFINED);
        preparing.shaders_built_at_draw = r.shaders_built_at_draw;
        preparing.pipelines_linked = r.pipelines_linked;
        preparing.pipelines_optimised = r.pipelines_optimised;
        preparing.report_preparing();
    }
    if r.world_frames > 1 {
        let seconds = r.world_seconds;
        log_info!(
            "renderer: %llu world frames in the %.1f s from the start of play to the last: %.1f a second in play",
            r.world_frames,
            seconds,
            if seconds > 0.0 { (r.world_frames - 1) as f64 / seconds } else { 0.0 }
        );
    }
    if r.submits != 0 {
        log_info!(
            "renderer: %llu submissions, %llu us of waiting each, in %llu segments more: %llu at the %llu points the command processor reported work done, %llu because the image pool was about to copy over a texture the segment samples",
            r.submits,
            t.fence / r.submits / 1000,
            r.segments,
            r.segments.wrapping_sub(r.stream_write_segments),
            r.completions,
            r.stream_write_segments
        );
    }
    report_recorder();
    if r.prewarm_wanted != 0 {
        log_info!(
            "renderer: %u of %u pipelines were ready before the first draw (%llu us)",
            r.prewarmed,
            r.prewarm_wanted,
            r.prewarm_microseconds
        );
    }
    log_info!(
        "renderer: %llu ms translating shaders, %llu ms compiling pipelines (%llu us each)",
        r.translate_microseconds / 1000,
        r.compile_microseconds / 1000,
        r.compile_microseconds.checked_div(r.pipeline_count).unwrap_or(0)
    );
    for line in &r.target_summary {
        log_info!("renderer: %s", line.as_c_str());
    }
    if r.resolve_images_made != 0 {
        log_info!(
            "renderer: %llu resolve destination images made (each after the first costs a device wait)",
            r.resolve_images_made
        );
    }
    if r.depth_resolve_wraps != 0 {
        log_info!(
            "renderer: %llu depth resolves started past their surface's edge, refused as empty (%llu past its height only, not Xenia's rule)",
            r.depth_resolve_wraps,
            r.depth_resolve_wraps_below
        );
    }
    let c = &r.resolve_clears;
    if c.any() {
        log_info!(
            "renderer: clears after a resolve by Xenia's rectangle: %llu of a part of the image, %llu of the whole, %llu outside it, %llu of an empty rectangle; %llu colour clears of a depth resolve not made, %llu with Xenia's tile range empty",
            c.part,
            c.whole,
            c.outside,
            c.nothing,
            c.colour_of_depth_resolve,
            c.no_tiles
        );
    }
    if let Some(frames) = core::num::NonZeroU64::new(r.frames) {
        // Per frame, because the arena is per frame.
        log_info!("renderer: frame arena peak %u KB of %u KB, per frame:", r.arena_peak >> 10, r.arena_bytes >> 10);
        for (kind, name) in ARENA_NAMES.iter().enumerate() {
            let name = c_string(*name);
            log_info!(
                "           %-12s %6llu KB copied, %6llu KB reused",
                name.as_c_str(),
                (r.arena_written[kind] / frames) >> 10,
                (r.arena_reused[kind] / frames) >> 10
            );
        }
        #[cfg(feature = "diagnostics")]
        super::arena_census::report(frames.get());
    }
    if r.verts_matched != 0 {
        log_info!(
            "renderer: %llu draws ran the shader MW2_TRACE_SHADER_VERTS named, %llu of them where the dump was asked for",
            r.verts_matched,
            r.verts_traced
        );
    }
    if r.arena_overflows != 0 {
        log_info!("renderer: the frame arena overflowed %llu times", r.arena_overflows);
    }
    let wide = super::renderer_arena::WIDE_RECTANGLE_LISTS.load(std::sync::atomic::Ordering::Relaxed);
    if wide != 0 {
        log_info!("renderer: %llu rectangle lists past 16384 rectangles drawn with 32-bit indices", wide);
    }
    for &(count, why) in &r.skipped {
        log_info!("renderer:   %8llu draws skipped: %s", count, why);
    }
}

#[cfg_attr(not(feature = "diagnostics"), allow(dead_code, reason = "the report and the diagnostic keys' alone"))]
fn report_gathered() {
    let made = super::renderer_draw::gathered();
    let load = |counter: &std::sync::atomic::AtomicU64| counter.load(Ordering::Relaxed);
    if load(&made.draws) != 0 {
        log_info!(
            "renderer: %llu draws made, %llu of them in %llu runs made with one call each, %llu on their own",
            load(&made.draws),
            load(&made.in_runs),
            load(&made.runs),
            load(&made.alone)
        );
    }
}

#[cfg_attr(not(feature = "diagnostics"), allow(dead_code, reason = "the report and the diagnostic keys' alone"))]
fn report_capture() {
    super::capture::report();
}

#[cfg_attr(not(feature = "diagnostics"), allow(dead_code, reason = "the report and the diagnostic keys' alone"))]
fn report_recorder() {
    recorder::report();
}
