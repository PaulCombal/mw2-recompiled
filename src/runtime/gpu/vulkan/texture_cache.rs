// Guest textures as Vulkan images (docs/textures.md says why each rule is what it is):
// read_texture's linear, host-endian bytes staged into a VkImage,
// with a view and a sampler built from the same fetch constant, each written once into set
// 1 of the pipeline layout, which holds every texture the cache has (bindings.rs).
//
// A title that rewrites a texture in place is noticed: the cache is keyed on the six dwords
// of the fetch constant, which do not change when only the bytes do, so a bound texture's
// memory is signed and re-staged if the signature moved -- every frame while it is
// changing, backing off to every sixteenth while it is not.
//
// Ids: an image's place in `images`, one-based, in bits 0-31, and its sampler's in the
// sampler list, one-based (0 for the image's own), in bits 32-61. An image's element in
// set 1 is its id plus two, after the stand-in of each kind; a draw's record holds that
// element and the sampler's slot above bit 16 (handle). An id is reused only after
// release, which waits until the submission the image was last bound in is completed and
// 10 s have passed since that submission began (UNUSED_NANOSECONDS), so no submission still
// on the GPU names it; adopt rewrites an entry only when its view
// changed, forget_views forgets every borrowed view with the device idle, and a sampler
// lives until shutdown.
//
// One thread: the ring consumer, which owns the renderer that owns this.
#![deny(unsafe_op_in_unsafe_fn)]

use super::device::Vk;
use super::format_support;
use super::image_memory::{ImageMemory, Range};
use super::renderer_state::FetchWords;
use super::texture_convert::{self, Convert};
use super::texture_formats as formats;
use super::texture_parts;
use super::tracked::{self, TrackedImage};
use super::util;
use crate::log::{log_info, log_warn};
use crate::runtime::bridge;
use crate::runtime::gpu::hash::WordsBuild;
use crate::runtime::gpu::shader::bindings;
use crate::runtime::gpu::texture::{self, DIMENSION_CUBE, Level, MAX_LEVELS, TextureFetch, TextureHeader};
use ash::vk;
use ash::vk::Handle;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::ffi::{CStr, c_void};
use std::ptr;

/// "Nothing uploaded", never a valid id.
pub const NONE: u64 = 0;
/// The submissions whose uploads can be open at once (renderer_state's FRAME_SLOTS at most).
pub const UPLOAD_SLOTS: u32 = 4;

// Sixteen frames is a quarter of a second at sixty: long enough to be cheap, short enough
// that a texture that starts moving is caught before it shows.
const SIGN_PERIOD_MAX: u64 = 16;
// What a submission's staging starts at and grows to (MW2_STAGING_LEAST_KB lowers the
// least in a diagnostics build).
const STAGING_LEAST: u64 = 8 << 20;
const STAGING_MOST: u64 = 256 << 20;
// How long an image goes unbound before it may be let go: ten seconds from the start of
// the submission it was last bound in, and that submission finished on the GPU (so made by
// the recorder) -- which is what lets its id, and its entry in the bindless set, be given
// to another image. Xenia's rule (texture_cache.cc: MarkAsUsed stamps a texture with
// current_submission_index_ and current_submission_time_, BeginSubmission's clock reading;
// CompletedSubmissionUpdated lets it go once last_usage_submission_index <= the completed
// submission and last_usage_time + the lifetime <= current_time), by time rather than
// frames, so the frame rate does not change it. Xenia's lifetime is 30 s and applies past a
// memory limit; this one is the plan's 10 s.
const UNUSED_NANOSECONDS: u64 = 10_000_000_000;
// MW2_DETERMINISTIC's clock for that: a frame is 1/60 s, a submission begun in frame n
// begins at n/60 s and the sweep after frame n looks at (n + 1)/60 s, so an image goes after
// 600 frames, as it did by frames.
#[cfg(feature = "diagnostics")]
const DETERMINISTIC_FRAME_NANOSECONDS: u64 = 16_666_667;
// The cache is looked over once in this many frames, the images a slice a frame.
const SWEEP_FRAMES: u64 = 64;
// The images released at a frame's end are destroyed for at most this long a frame.
const DESTROY_NANOSECONDS: u64 = 500_000;
// The re-reads a segment's end makes at most: of rewritten textures, and of empty ones.
const REWRITTEN_PER_SEGMENT: usize = 256;
const EMPTY_PER_SEGMENT: usize = 64;

// An id is an image and, above bit 32, the sampler it is bound with.
const IMAGE_BITS: u64 = 0xFFFF_FFFF;
const SAMPLER_SHIFT: u32 = 32;

// stutters.rs's costs and report.rs's time the texture work is counted under.
const STUTTER_NEW_TEXTURES: usize = 2;
const STUTTER_REFRESHED_TEXTURES: usize = 3;
const REPORT_TEXTURES: usize = 3;

/// gpu::TextureData: what read_texture gives, its bytes with it.
#[derive(Clone)]
pub struct TextureData {
    pub header: TextureHeader,
    pub bytes: Vec<u8>,
}

impl TextureData {
    pub fn levels(&self) -> &[Level] {
        &self.header.levels[..(self.header.level_count as usize).min(MAX_LEVELS)]
    }

    /// read_texture into bytes of its own.
    ///
    /// # Safety
    /// As read_texture's: `base` and `mips` null or readable as far as the layout reaches.
    pub unsafe fn read(fetch: &TextureFetch, base: *const u8, mips: *const u8) -> TextureData {
        let mut bytes = Vec::new();
        // SAFETY: the caller's ranges; reserve hands back `size` zeroed bytes it owns.
        let header = unsafe {
            texture::read_texture(fetch, base, mips, &mut |size| {
                bytes = vec![0u8; size];
                bytes.as_mut_ptr()
            })
        };
        TextureData { header, bytes }
    }
}

/// What the cache asks of the shared pipeline's device (shaders/src/pipeline.rs), read at
/// initialise.
#[derive(Clone)]
pub struct TextureDevice {
    pub device: u64,
    pub physical: u64,
    pub queue: u64,
    pub queue_family: u32,
    pub samplers: u32,
    pub max_anisotropy: f32,
    pub non_seamless_cubes: bool,
    pub texture_set_layout: u64,
    pub name: std::ffi::CString,
}

impl Default for TextureDevice {
    fn default() -> Self {
        TextureDevice {
            device: 0,
            physical: 0,
            queue: 0,
            queue_family: 0,
            samplers: 0,
            max_anisotropy: 0.0,
            non_seamless_cubes: false,
            texture_set_layout: 0,
            name: std::ffi::CString::default(),
        }
    }
}

/// The shared pipeline's device, started first (headless) if it is not running; None when
/// it will not start.
fn textures_device() -> Option<TextureDevice> {
    use mw2_shaders::pipeline as p;
    // SAFETY: a headless start: the pipeline brings up and owns its own device.
    if !p::ready() && !unsafe { p::initialise(None) } {
        return None;
    }
    Some(TextureDevice {
        device: p::device().as_raw(),
        physical: p::physical_device().as_raw(),
        queue: p::queue().as_raw(),
        queue_family: p::queue_family(),
        samplers: p::samplers(),
        max_anisotropy: p::max_anisotropy(),
        non_seamless_cubes: p::non_seamless_cubes(),
        texture_set_layout: p::set_layout(bindings::TEXTURE_SET).as_raw(),
        name: p::device_name(),
    })
}

#[derive(Clone)]
struct Image {
    image: vk::Image,
    memory: Range,
    view: vk::ImageView,
    // Borrowed from the sampler cache; what an id that names no sampler of its own is
    // sampled with, and its place in the bindless set.
    sampler: vk::Sampler,
    sampler_slot: u32,
    // The view the bindless set holds at this id; null when it holds none that is still
    // good.
    written: vk::ImageView,
    format: vk::Format,
    width: u32,
    height: u32,
    blocks_wide: u32,
    blocks_high: u32,
    block_width: u32,
    block_height: u32,
    swizzle: u32,
    bytes: u64,
    // Six for a cube map; for a volume the depth, and the image a real 3D one.
    layers: u32,
    volume: bool,
    signature: u64,
    checked_frame: u64,
    // How many frames to leave before signing this one again.
    sign_period: u64,
    queued_segment: u64, // the segment it was last put on rewritten_this_segment
    // When its memory is watched, the stamp it was read under; zero when not watched.
    watch_stamp: u64,
    staged: u32,        // times staged; the draw list's version of it
    bound_segment: u64, // the last segment a draw of which bound it
    rewritten: bool,
    address: u32, // physical
    xenos_format: u32,
    // The mip chain's memory, signed together with the base's; zero with no chain.
    mip_address: u32,
    mip_bytes: usize,
    // Where each level is in the staged bytes.
    levels: Vec<Level>,
    fetch_words: [u32; 6],
    // Every fetch constant that names this image.
    names: Vec<[u32; 6]>,
    content: [u32; 6], // its key in by_content
    min_level: u32,    // the lowest level it holds a picture for
    // Read from a block that looked never written, and when that was first seen.
    from_an_empty_block: bool,
    counted_drawn: bool,
    empty_since: u64,
    source_bytes: usize,
    borrowed: bool, // the view belongs to someone else
    pinned: bool,   // never evicted: the magenta marker
    last_bound: u64,
    // Xenia's last_usage_submission_index and last_usage_time (UNUSED_NANOSECONDS).
    last_submission: u64,
    last_used_at: u64,
    // How the guest's bytes become the host format's (format_support.rs's texture_host).
    convert: Convert,
    // The host format can be filtered linearly; otherwise its samplers are point ones.
    filterable: bool,
    // What the image was last used for (tracked.rs).
    track: TrackedImage,
    // Diagnostics: where the extents before they followed the reads (texture::row_extents)
    // named less of a range than the reader reads, those shorter ranges and their
    // signature, to count the changes only the rest of the ranges held. Zero bytes when
    // they named as much, or without MW2_COUNT_PAST_ROWS=1.
    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code, reason = "a diagnostics count's alone"))]
    narrow_bytes: usize,
    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code, reason = "a diagnostics count's alone"))]
    narrow_mip_bytes: usize,
    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code, reason = "a diagnostics count's alone"))]
    narrow_signature: u64,
    // Cut into parts once it has changed and is watched, to be read again in parts
    // (texture_parts.rs); and whether the signature is older than what was uploaded since,
    // which a whole read then takes for a change.
    parts: Option<Box<texture_parts::Parts>>,
    signature_stale: bool,
}

impl Default for Image {
    fn default() -> Self {
        Image {
            image: vk::Image::null(),
            memory: Range::default(),
            view: vk::ImageView::null(),
            sampler: vk::Sampler::null(),
            sampler_slot: 0,
            written: vk::ImageView::null(),
            format: vk::Format::UNDEFINED,
            width: 0,
            height: 0,
            blocks_wide: 0,
            blocks_high: 0,
            block_width: 1,
            block_height: 1,
            swizzle: 0x688,
            bytes: 0,
            layers: 1,
            volume: false,
            signature: 0,
            checked_frame: u64::MAX,
            sign_period: 1,
            queued_segment: u64::MAX,
            watch_stamp: 0,
            staged: 0,
            bound_segment: u64::MAX,
            rewritten: false,
            address: 0,
            xenos_format: 0,
            mip_address: 0,
            mip_bytes: 0,
            levels: Vec::new(),
            fetch_words: [0; 6],
            names: Vec::new(),
            content: [0; 6],
            min_level: 0,
            from_an_empty_block: false,
            counted_drawn: false,
            empty_since: 0,
            source_bytes: 0,
            borrowed: false,
            pinned: false,
            last_bound: 0,
            last_submission: 0,
            last_used_at: 0,
            convert: Convert::None,
            filterable: true,
            track: TrackedImage::NEW,
            narrow_bytes: 0,
            narrow_mip_bytes: 0,
            narrow_signature: 0,
            parts: None,
            signature_stale: false,
        }
    }
}

#[derive(Clone, Copy)]
struct StagingBuffer {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    mapped: *mut u8,
    size: u64,
}

impl Default for StagingBuffer {
    fn default() -> Self {
        StagingBuffer { buffer: vk::Buffer::null(), memory: vk::DeviceMemory::null(), mapped: ptr::null_mut(), size: 0 }
    }
}

/// The uploads of one renderer slot, and the staging they read from: a command buffer for
/// each segment of the submission (end_segment), each submitted just ahead of its
/// segment's draws. A submission's uploads are sub-allocated from one buffer, grown for
/// the next time round when they overflow it; what overflows gets a buffer of its own
/// until then.
#[derive(Default)]
struct UploadSlot {
    pool: vk::CommandPool,
    commands: Vec<vk::CommandBuffer>, // made as segments need them
    segment: usize,                   // the one being recorded into
    recording: bool,
    staging: StagingBuffer,
    used: u64,
    wanted: u64,
    overflow: Vec<StagingBuffer>,
}

#[derive(Clone, Copy, Default)]
struct Sampler {
    sampler: vk::Sampler,
    slot: u32,
}

#[derive(Clone, Copy)]
struct LateRead {
    id: u64,
    frame: u64,
}

/// The device and what was read of it at initialise.
#[derive(Default)]
struct Dev {
    device: vk::Device,
    physical: vk::PhysicalDevice,
    queue: vk::Queue,
    memory: vk::PhysicalDeviceMemoryProperties,
    max_dimension: u32,
    // The most a cube's side, a volume's sides and depth, and an array's layers may be.
    max_cube: u32,
    max_3d: u32,
    max_layers: u32,
    info: TextureDevice,
}

/// Failures: how many, why, and the last one's text (Fail).
#[derive(Default)]
struct Failures {
    failures: u32,
    reasons: BTreeMap<&'static CStr, u32>,
    last_error: Option<&'static CStr>,
}

impl Failures {
    #[cold]
    fn fail(&mut self, what: &'static CStr, error: Option<&mut &'static CStr>) -> bool {
        self.last_error = Some(what);
        self.failures = self.failures.wrapping_add(1);
        let count = self.reasons.entry(what).or_insert(0);
        *count = count.wrapping_add(1);
        if let Some(error) = error {
            *error = what;
        }
        false
    }
}

/// The uploads: a submission's slots, and the command buffer and fence of one made alone.
struct Uploads {
    commands: vk::CommandPool,
    command: vk::CommandBuffer,
    fence: vk::Fence,
    slots: [UploadSlot; UPLOAD_SLOTS as usize],
    open: Option<usize>, // open between begin_uploads and end_submission
    batched: u64,
    alone: u64,
    overflows: u64,
    // MW2_STAGING_LEAST_KB lowers it (initialise), and it stays lowered.
    staging_least: u64,
    // Where each slot's staging was filled up to at its last segment's end (the trace).
    staging_hashed: [u64; UPLOAD_SLOTS as usize],
}

impl Default for Uploads {
    fn default() -> Self {
        Uploads {
            commands: vk::CommandPool::null(),
            command: vk::CommandBuffer::null(),
            fence: vk::Fence::null(),
            slots: Default::default(),
            open: None,
            batched: 0,
            alone: 0,
            overflows: 0,
            staging_least: STAGING_LEAST,
            staging_hashed: [0; UPLOAD_SLOTS as usize],
        }
    }
}

/// Set 1: every image two past its id, after the stand-ins, every sampler at its slot.
#[derive(Default)]
struct Bindless {
    pool: vk::DescriptorPool,
    set: vk::DescriptorSet,
    written: u64, // descriptors written
}

/// The frame and segment count and the re-read lists (State's frame, segment, epoch and
/// what is read again at a segment's end).
#[derive(Default)]
struct Tracking {
    frame: u64,
    segment: u64, // end_segment calls so far
    epoch: u64,
    refreshed: u64,
    resigned: u64,
    invalidated_by_writes: u64,
    sign_nanos: u64, // spent signing, all textures
    unchanged_by_watch: u64,
    // Textures uploaded from a block that looked empty, and textures the guest has been
    // seen to rewrite and a draw of the segment binds.
    empty_this_segment: Vec<u64>,
    rewritten_this_segment: Vec<u64>,
    late_restages: Vec<u64>, // see take_late_restages
    segments_ended: u64,
    segments_with_uploads: u64,
    ending_submission: bool,
    reread_rewritten: u64,
    changed_after_first_bind: u64,
    late_reads: Vec<LateRead>,
    late_reads_bound_again: u64,
    late_reads_on_last_use: u64,
    empty_drawn_in_world: u64,
    refilled_before_submit: u64,
    still_empty_at_submit: u64,
    // Lookups that had failed, made again at the next frame's first bind (failed_keys): those
    // that succeeded, and those that failed again.
    failures_made: u64,
    failures_failed_again: u64,
    // Diagnostics, MW2_COUNT_PAST_ROWS=1: re-reads and releases of a changed texture whose
    // change lay wholly past what the extents named before they followed the reads
    // (Image::narrow_bytes).
    changed_past_rows: u64,
    // The images' age (UNUSED_NANOSECONDS): the submission being recorded and when it
    // began (Xenia's current_submission_index_ and current_submission_time_), the last the
    // GPU has finished as of its beginning, and the time the sweep compares with, taken at
    // each new frame.
    submission: u64,
    submission_began: u64,
    completed: u64,
    now: u64,
}

/// The texture cache.
#[derive(Default)]
pub struct TextureCache {
    dev: Dev,
    up: Uploads,
    fails: Failures,
    bind: Bindless,
    track: Tracking,
    memory: ImageMemory,

    // MW2_MARK_EMPTY=1: the id of a flat magenta texture.
    magenta: u64,
    marked_empty: u64,
    linearised_refused: u64, // no sRGB form of the format to linearise with
    // S13a's fallbacks taken (format_support.rs): images converted for a format the device
    // cannot sample, and images given point samplers for one it cannot filter.
    converted: u64,
    point_only: u64,
    // Ids are one-based indices into this, so zero is never a texture.
    images: Vec<Image>,
    // by_fetch has the id for all six dwords, which is the image and the sampler;
    // by_content has the image for the dwords less the sampler's bits (content_of).
    by_fetch: HashMap<FetchWords, u64, WordsBuild>,
    by_content: HashMap<FetchWords, u64, WordsBuild>,
    // The lookups by_fetch answers NONE for, failed this frame: forgotten at the next frame's
    // start so their next bind makes them again; a fetch bound every draw fails once a frame,
    // not every draw. The runtime's rule: Xenia caches no failure by key but leaves a fetch
    // slot's binding empty, looked up again when the slot's key changes, at BeginFrame, when
    // trimming destroys a texture or a watched one is written (texture_cache.cc
    // RequestTextures, ResetTextureBindings); its first attempt in a frame is made here too
    // (docs/textures.md). retrying holds the keys the last frame start forgot, to count what
    // their next lookup comes to.
    failed_keys: Vec<FetchWords>,
    retrying: HashSet<FetchWords, WordsBuild>,
    free_ids: Vec<u64>,                // slots of images to use again
    sampler_list: Vec<Sampler>,        // an id's sampler, by its index less one
    sampler_index: BTreeMap<u64, u32>, // sampler key -> that index
    adopted: BTreeMap<u64, u64>,       // caller's key -> id
    samplers: BTreeMap<u64, Sampler>,
    // Images nothing names any more, waiting to be destroyed a few a frame.
    doomed: VecDeque<Image>,
    fallback: Image,
    fallback_cube: Image,
    fallback_3d: Image,
    kind_mismatches: u64,

    uploads: u32,
    uploaded_bytes: u64,
    live_bytes: u64,
    budget: u64,
    evicted: u64,
    evicted_bytes: u64,
    eviction_passes: u64,
    stale: u64,
    stale_bytes: u64,
    stale_not_really: u64,
    shared: u64,
    // Re-reads in parts (texture_parts.rs): the scratch they read into and stage from, and
    // what they came to.
    parts: PartsWork,
    // Diagnostics: the idle images let go, and the youngest of them (S14d's measure).
    #[cfg(feature = "diagnostics")]
    let_go: LetGo,
}

/// The re-reads in parts' buffers, kept from one to the next, and their counts.
#[derive(Default)]
struct PartsWork {
    scratch: Vec<u8>,
    bytes: Vec<u8>,
    regions: Vec<vk::BufferImageCopy>,
    written: Vec<bool>,
    spans: Vec<usize>,
    // Textures cut, re-reads in parts (of which read whole, most of it written), parts
    // read and parts uploaded (and their bytes).
    cut: u64,
    rereads: u64,
    whole: u64,
    read: u64,
    uploaded: u64,
    uploaded_bytes: u64,
    // MW2_CHECK_TEXTURE_PARTS: re-reads checked, and the parts whose bytes differed from a
    // whole read, with no page of theirs written since (missed) or with one (the title
    // wrote it after the read).
    checked: u64,
    missed_parts: u64,
    missed_bytes: u64,
    raced_parts: u64,
    // ...and the bytes the copies' regions put elsewhere than the parts' bytes say.
    misplaced_bytes: u64,
    // ...and the times the running count of empty spans differed from a count.
    miscounted: u64,
    // By texture (its address, sizes and format): its parts, re-reads in parts, parts
    // read and parts uploaded. The images are gone by the report.
    by_texture: HashMap<(u32, u32, u32, u32), [u64; 5]>,
}

/// MW2_CHECK_TEXTURE_PARTS=1 (diagnostics): every re-read in parts is followed by a whole
/// read, and what was uploaded compared with it.
fn check_parts() -> bool {
    static ON: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| cfg!(feature = "diagnostics") && crate::env::flag(c"MW2_CHECK_TEXTURE_PARTS"));
    *ON
}

/// The idle images let go (released or evicted): how many, and the least time since the
/// submission each was last used in began, and the fewest frames since its last bind.
#[cfg(feature = "diagnostics")]
#[derive(Default)]
struct LetGo {
    count: u64,
    nanoseconds: u64,
    frames: u64,
}

// SAFETY: the mapped staging pointers are this cache's own memory, used by whichever one
// thread owns the cache (the ring consumer).
unsafe impl Send for TextureCache {}

// ---- guest memory written on another thread, and the cache after its shutdown ----

/// MemoryWritten from a thread other than the ring consumer's (the image pool's move made at
/// once on the guest thread: whenever nothing consumes the stream, and in play too when its
/// destination has no word-aligned physical address for the copy draw to name): the ranges,
/// physical, queued for the consumer, which applies them at the start of its next batch
/// (apply_written_elsewhere), so that no guest thread walks the cache's images while the
/// consumer could be changing them. ANY is true exactly while the list holds something, both changed under
/// the lock; the consumer reads it without, so a range queued during a batch is applied at
/// the one after.
static WRITTEN_ELSEWHERE: std::sync::Mutex<Vec<(u32, u32)>> = std::sync::Mutex::new(Vec::new());
static WRITTEN_ELSEWHERE_ANY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
// How many were queued and how many the consumer applied, for the report: a range queued
// in play is applied at the next batch, one queued after the consumer's last batch (a move
// during the exit, after gpu::shutdown) never, so the difference is the latter only.
static WRITTEN_ELSEWHERE_QUEUED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static WRITTEN_ELSEWHERE_APPLIED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn written_elsewhere() -> std::sync::MutexGuard<'static, Vec<(u32, u32)>> {
    WRITTEN_ELSEWHERE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// vk::textures::MemoryWritten from any thread but the consumer: queued for it.
pub fn memory_written_elsewhere(physical: u32, size: u32) {
    use std::sync::atomic::Ordering;
    let mut list = written_elsewhere();
    list.push((physical, size));
    WRITTEN_ELSEWHERE_QUEUED.fetch_add(1, Ordering::Relaxed);
    WRITTEN_ELSEWHERE_ANY.store(true, Ordering::Release);
}

impl TextureCache {
    /// The consumer, at the start of a batch: what other threads queued, in their order.
    #[inline]
    pub fn apply_written_elsewhere(&mut self) {
        use std::sync::atomic::Ordering;
        if !WRITTEN_ELSEWHERE_ANY.load(Ordering::Acquire) {
            return;
        }
        self.apply_written_elsewhere_now();
    }

    #[cold]
    #[inline(never)]
    fn apply_written_elsewhere_now(&mut self) {
        use std::sync::atomic::Ordering;
        let queued = {
            let mut list = written_elsewhere();
            WRITTEN_ELSEWHERE_ANY.store(false, Ordering::Relaxed);
            std::mem::take(&mut *list)
        };
        for (physical, size) in queued {
            self.memory_written(physical, size);
            WRITTEN_ELSEWHERE_APPLIED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The cache after its shutdown, kept for the report written after it. None before (the
/// fault path's report, which
/// reads statics alone, prints no texture lines).
static AFTER_SHUTDOWN: std::sync::Mutex<Option<TextureCache>> = std::sync::Mutex::new(None);

/// The renderer's shutdown hands its cache over, its device's objects destroyed.
pub fn keep_for_report(cache: TextureCache) {
    *AFTER_SHUTDOWN.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(cache);
}

#[cfg_attr(not(feature = "diagnostics"), allow(dead_code, reason = "the report and the diagnostic keys' alone"))]
/// vk::textures::Report after the shutdown. Never waits: the fault path can reach here
/// from a signal handler on the thread that holds the lock.
pub fn report_after_shutdown() {
    use std::sync::atomic::Ordering;
    let queued = WRITTEN_ELSEWHERE_QUEUED.load(Ordering::Relaxed);
    let applied = WRITTEN_ELSEWHERE_APPLIED.load(Ordering::Relaxed);
    let held = match AFTER_SHUTDOWN.try_lock() {
        Ok(held) => Some(held),
        Err(std::sync::TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    };
    if let Some(cache) = held.as_ref().and_then(|held| held.as_ref()) {
        cache.report();
    }
    if cfg!(feature = "diagnostics") && queued != 0 {
        log_info!(
            "textures: %llu guest writes from other threads queued, %llu applied at the consumer's next batch",
            queued,
            applied
        );
    }
}

// ---- what lies outside: the clock, the recorder, the watch, the capture, the image pool ----

mod outside {
    pub fn now() -> u64 {
        crate::runtime::clock::nanoseconds()
    }

    pub fn drain() {
        super::super::recorder::drain();
    }

    pub fn describe_moves_around(physical: u32, size: u32, frames: u64, out: &mut [u8]) {
        crate::runtime::gpu::image_move::describe_moves_around(physical, size, frames, out);
    }
}

/// stutters::Timed and report::Timed over a scope: the time spent goes to both, each when
/// it is on.
struct Timed {
    stutter: usize,
    from: Option<u64>,
    began: Option<u64>,
}

impl Timed {
    #[inline]
    fn new(stutter: usize) -> Timed {
        #[cfg(feature = "diagnostics")]
        let from = crate::runtime::stutters::on().then(crate::runtime::clock::nanoseconds);
        #[cfg(not(feature = "diagnostics"))]
        let from = None;
        let began = crate::runtime::report::on()
            .then(|| crate::runtime::report::timed_begin(crate::runtime::clock::nanoseconds(), REPORT_TEXTURES));
        Timed { stutter, from, began }
    }
}

impl Drop for Timed {
    #[inline]
    fn drop(&mut self) {
        #[cfg(feature = "diagnostics")]
        if let Some(from) = self.from {
            crate::runtime::stutters::add(self.stutter, crate::runtime::clock::nanoseconds().wrapping_sub(from));
        }
        #[cfg(not(feature = "diagnostics"))]
        let _ = (self.stutter, self.from);
        if let Some(began) = self.began {
            crate::runtime::report::timed_end(crate::runtime::clock::nanoseconds(), REPORT_TEXTURES, began);
        }
    }
}

/// MW2_FLASH_HUNT=1 (tools/flash_hunt.sh): every texture that reaches the screen with
/// nothing in it gets its line, not only the first few.
fn flash_hunt() -> bool {
    static ON: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| cfg!(feature = "diagnostics") && crate::env::flag(c"MW2_FLASH_HUNT"));
    *ON
}

/// MW2_MARK_EMPTY=1: every world texture uploaded from a block with nothing in it painted
/// flat magenta, so what the counter counts can be seen.
fn mark_empty() -> bool {
    static ON: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| cfg!(feature = "diagnostics") && crate::env::flag(c"MW2_MARK_EMPTY"));
    *ON
}

/// MW2_COUNT_PAST_ROWS=1: count the changes caught only past what the extents named before
/// they followed the reads (TextureCache::note_changed). Off by default: it signs most new
/// textures a second time inside the timed upload, which the release build does not do.
#[cfg(feature = "diagnostics")]
fn count_past_rows() -> bool {
    static ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| crate::env::flag(c"MW2_COUNT_PAST_ROWS"));
    *ON
}

/// The host format a texture of the guest format's equivalent `direct` is made in, the
/// conversion it needs, and whether an sRGB image was asked for (`linearise`) and refused.
/// Where the device cannot sample the equivalent, Xenia's fallback (format_support.rs); a
/// fetch constant asking for the stored value to be linearised gets an sRGB image, which
/// does that in the sampler from the same bytes, where the device can sample one.
fn host_format(
    direct: i32,
    linearise: bool,
    features: impl Fn(vk::Format) -> vk::FormatFeatureFlags,
) -> (i32, Convert, bool) {
    let (mut format, convert) = format_support::texture_host(direct, &features);
    let mut refused = false;
    if linearise {
        let srgb = formats::srgb_format_for(format);
        if srgb != vk::Format::UNDEFINED.as_raw()
            && features(vk::Format::from_raw(srgb)).contains(vk::FormatFeatureFlags::SAMPLED_IMAGE)
        {
            format = srgb;
        } else {
            refused = true;
        }
    }
    (format, convert, refused)
}

fn explain(r: vk::Result) -> &'static CStr {
    match r {
        vk::Result::SUCCESS => c"ok",
        vk::Result::ERROR_OUT_OF_HOST_MEMORY => c"out of host memory",
        vk::Result::ERROR_OUT_OF_DEVICE_MEMORY => c"out of device memory",
        vk::Result::ERROR_OUT_OF_POOL_MEMORY => c"descriptor pool full",
        vk::Result::ERROR_FORMAT_NOT_SUPPORTED => c"format not supported",
        _ => c"failed",
    }
}

/// The kind a view was made as, in the translation's texture_kinds' terms.
#[inline]
fn kind_of(image: &Image) -> u8 {
    if image.volume {
        2
    } else if image.layers > 1 {
        1
    } else {
        0
    }
}

/// An image's place in the bindless set's array: after the stand-in of each kind, so its
/// id, one-based, plus two.
#[inline]
fn element_of(id: u64) -> u32 {
    ((id & IMAGE_BITS) as u32).wrapping_add(bindings::IMAGE_KINDS - 1)
}

/// Every byte of the texture's memory, four lanes wide to keep the multiplies off one
/// dependency chain. A change to any single word is always seen, since each step is a
/// bijection of the lane's state.
///
/// # Safety
/// `base` readable for `bytes` (guest memory, which guest threads may be writing: read
/// through the pointer, never as a slice).
#[inline]
pub unsafe fn signature_of(base: *const u8, bytes: usize) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01B3;
    let mut lane: [u64; 4] =
        [0xCBF2_9CE4_8422_2325, 0x8422_2325_CBF2_9CE4, 0x9E37_79B9_7F4A_7C15, 0x7F4A_7C15_9E37_79B9];
    let mut at = 0usize;
    while at + 32 <= bytes {
        // SAFETY: at + 32 <= bytes, the caller's range.
        let w: [u64; 4] = unsafe { ptr::read_unaligned(base.add(at).cast::<[u64; 4]>()) };
        for i in 0..4 {
            lane[i] = (lane[i] ^ u64::from_le(w[i])).wrapping_mul(PRIME);
        }
        at += 32;
    }
    while at < bytes {
        // SAFETY: at < bytes.
        let byte = unsafe { ptr::read(base.add(at)) };
        lane[at & 3] = (lane[at & 3] ^ u64::from(byte)).wrapping_mul(PRIME);
        at += 1;
    }
    let mut hash = (bytes as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    for value in lane {
        hash = (hash ^ value).wrapping_mul(PRIME) ^ (value >> 29);
    }
    hash
}

/// How much of a block reads as never written, in percent: spans of sixty-four bytes that
/// are zero all through (a dark or black compressed texture is full of zero bytes, quite
/// correctly; memory nothing has written is zero in long unbroken stretches).
pub fn empty_percent(bytes: &[u8]) -> u8 {
    if bytes.len() < 64 {
        return 0;
    }
    let (mut empty, mut spans) = (0usize, 0usize);
    for span in bytes.chunks_exact(64) {
        if span.iter().all(|&b| b == 0) {
            empty += 1;
        }
        spans += 1;
    }
    (empty * 100).checked_div(spans).map_or(0, |percent| percent as u8)
}

/// The fetch constant less what only the sampler reads: the clamps, the filters and the
/// anisotropy, and the border colour.
#[inline]
pub fn content_of(fetch: &TextureFetch) -> [u32; 6] {
    let mut words = fetch.d;
    words[0] &= !(0x1FFu32 << 10); // ClampX, ClampY, ClampZ
    words[3] &= !(0x1FFu32 << 19); // MagFilter, MinFilter, MipFilter, Anisotropy
    words[5] &= !0x3u32; // BorderColour
    words
}

/// One sampler per distinct state: the fetch constant's own bits and the lowest level the
/// texture may be sampled at. `min_level` is the lowest level the image holds a picture
/// for; under BaseMap it is the base whenever the base was read.
pub fn sampler_key(fetch: &TextureFetch, mut min_level: u32) -> u64 {
    const BASE_MAP: u32 = 2;
    if fetch.mip_filter() == BASE_MAP && min_level != 0 && texture::extent_levels(fetch).base_address != 0 {
        min_level = 0;
    }
    u64::from(fetch.mag_filter())
        | (u64::from(fetch.min_filter()) << 2)
        | (u64::from(fetch.mip_filter()) << 4)
        | (u64::from(fetch.clamp_x()) << 6)
        | (u64::from(fetch.clamp_y()) << 9)
        | (u64::from(fetch.clamp_z()) << 12)
        | (u64::from(fetch.border_colour()) << 15)
        | (u64::from(min_level & 15) << 17)
        | (u64::from(fetch.anisotropy()) << 21)
        | (u64::from(fetch.dimension() == DIMENSION_CUBE) << 24)
}

fn address_mode(clamp: u32) -> vk::SamplerAddressMode {
    match clamp {
        0 => vk::SamplerAddressMode::REPEAT,
        1 => vk::SamplerAddressMode::MIRRORED_REPEAT,
        2 => vk::SamplerAddressMode::CLAMP_TO_EDGE,
        3 => vk::SamplerAddressMode::MIRROR_CLAMP_TO_EDGE,
        // "Halfway" clamps at the edge texel's centre, which Vulkan has no mode for.
        4 => vk::SamplerAddressMode::CLAMP_TO_EDGE,
        5 => vk::SamplerAddressMode::MIRROR_CLAMP_TO_EDGE,
        6 => vk::SamplerAddressMode::CLAMP_TO_BORDER,
        7 => vk::SamplerAddressMode::CLAMP_TO_BORDER,
        _ => vk::SamplerAddressMode::REPEAT,
    }
}

#[inline]
fn filter_of(filter: u32) -> vk::Filter {
    if filter == 0 { vk::Filter::NEAREST } else { vk::Filter::LINEAR }
}

/// A sampler key with every linear filter made point and no anisotropy, for an image whose
/// format cannot be filtered linearly (Xenia's GetSamplerParameters; format_support.rs).
#[inline]
pub fn point_only(key: u64, filterable: bool) -> u64 {
    if filterable {
        return key;
    }
    const MIP_LINEAR: u64 = 1 << 4;
    let mip = if (key >> 4) & 3 == 1 { MIP_LINEAR } else { 0 };
    // Mag 0-1, min 2-3, the mip filter when it is linear, anisotropy 21-23.
    key & !(0xF | mip | (7 << 21))
}

/// The host address of a texture's first byte, from the physical address its fetch
/// constant holds; null when it names no guest memory.
#[inline]
fn host_address(physical: u32) -> *const u8 {
    if physical == 0 {
        return ptr::null();
    }
    let va = crate::runtime::kernel::from_physical(physical);
    if va != 0 { crate::runtime::guest_memory::base().wrapping_add(va as usize).cast_const() } else { ptr::null() }
}

/// MW2_CAPTURE_GPU: the ranges a texture is about to be read from, each with the page
/// after it, as every capture has held them (the reader stays inside extents_of's ranges).
#[cfg(feature = "diagnostics")]
fn note_source(base: u32, base_bytes: usize, mips: u32, mip_bytes: usize) {
    let page = u64::from(crate::runtime::gpu::memory_watch::PAGE_BYTES);
    let note = |physical: u32, bytes: u64| {
        let va = crate::runtime::kernel::from_physical(physical);
        if va != 0 {
            bridge::stream::note_read(va, bytes);
        }
    };
    if base_bytes != 0 {
        note(base, base_bytes as u64 + page);
    }
    if mip_bytes != 0 {
        note(mips, mip_bytes as u64 + page);
    }
}

#[cfg(feature = "diagnostics")]
fn note_image(image: &Image) {
    note_source(image.address, image.source_bytes, image.mip_address, image.mip_bytes);
}

/// signature_of, timed into `nanos` in a diagnostics build.
#[inline]
unsafe fn timed_signature_of(at: *const u8, bytes: usize, nanos: Option<&mut u64>) -> u64 {
    match nanos {
        Some(nanos) if cfg!(feature = "diagnostics") => {
            let started = outside::now();
            // SAFETY: the caller's.
            let signature = unsafe { signature_of(at, bytes) };
            *nanos = nanos.wrapping_add(outside::now().wrapping_sub(started));
            signature
        }
        // SAFETY: the caller's.
        _ => unsafe { signature_of(at, bytes) },
    }
}

/// Both ranges a texture was read from, as one signature: a rewrite of the chain changes
/// what is sampled as surely as one of the base. Timed into `nanos` when given.
fn sign_source(base: u32, base_bytes: usize, mips: u32, mip_bytes: usize, mut nanos: Option<&mut u64>) -> u64 {
    let mut signature = 0u64;
    let at = if base_bytes != 0 { host_address(base) } else { ptr::null() };
    if !at.is_null() {
        // SAFETY: the guest's memory, read as far as the texture's extents reach.
        signature = unsafe { timed_signature_of(at, base_bytes, nanos.as_deref_mut()) };
    }
    let at = if mip_bytes != 0 { host_address(mips) } else { ptr::null() };
    if !at.is_null() {
        // SAFETY: as above.
        let chain = unsafe { timed_signature_of(at, mip_bytes, nanos) };
        signature = (signature ^ chain).wrapping_mul(0x0000_0100_0000_01B3).wrapping_add(1);
    }
    signature
}

fn sign_image(image: &Image, nanos: Option<&mut u64>) -> u64 {
    sign_source(image.address, image.source_bytes, image.mip_address, image.mip_bytes, nanos)
}

/// Diagnostics: the signature over the shorter ranges row_extents named (Image::narrow_bytes).
#[cfg(feature = "diagnostics")]
fn sign_narrow(image: &Image) -> u64 {
    sign_source(image.address, image.narrow_bytes, image.mip_address, image.narrow_mip_bytes, None)
}

/// The write watch on a texture's memory, taken before the memory is read: a write after
/// this point shows, one before it is in what is read. Zero when any of it cannot be
/// watched, which leaves the texture to its signature.
fn track_source(frame: u64, base: u32, base_bytes: usize, mips: u32, mip_bytes: usize) -> u64 {
    if !bridge::watch::enabled() || (base_bytes == 0 && mip_bytes == 0) {
        return 0;
    }
    let mut stamp = 0u64;
    for (address, bytes) in [(base, base_bytes), (mips, mip_bytes)] {
        if bytes == 0 {
            continue;
        }
        if crate::runtime::kernel::from_physical(address)
            != crate::runtime::guest_memory::PHYSICAL_BASE.wrapping_add(address)
        {
            return 0;
        }
        let one = bridge::watch::track(address, bytes as u32, frame);
        if one == 0 {
            return 0;
        }
        stamp = stamp.max(one);
    }
    stamp
}

fn track_image(frame: u64, image: &Image) -> u64 {
    track_source(frame, image.address, image.source_bytes, image.mip_address, image.mip_bytes)
}

fn unchanged(image: &Image) -> bool {
    image.watch_stamp != 0
        && (image.source_bytes == 0
            || bridge::watch::unwritten(image.address, image.source_bytes as u32, image.watch_stamp))
        && (image.mip_bytes == 0
            || bridge::watch::unwritten(image.mip_address, image.mip_bytes as u32, image.watch_stamp))
}

/// The texture as its memory holds it now, through the fetch constant it was uploaded from.
fn read_again(image: &Image) -> TextureData {
    let fetch = TextureFetch { d: image.fetch_words };
    // SAFETY: guest memory at the texture's own addresses, as the upload read it.
    unsafe { TextureData::read(&fetch, host_address(image.address), host_address(image.mip_address)) }
}

/// One region a level, from the bytes staged at `offset`. buffer_row_length is in texels
/// even for a block format, so it is the block count times the block width.
fn regions(image: &Image, offset: u64, out: &mut [vk::BufferImageCopy; MAX_LEVELS]) -> usize {
    let count = image.levels.len().min(MAX_LEVELS);
    for (level, (l, region)) in image.levels.iter().zip(out.iter_mut()).enumerate() {
        *region = vk::BufferImageCopy {
            buffer_offset: offset.wrapping_add(l.offset as u64),
            buffer_row_length: l.blocks_wide.wrapping_mul(image.block_width),
            buffer_image_height: l.blocks_high.wrapping_mul(image.block_height),
            image_subresource: vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: level as u32,
                base_array_layer: 0,
                layer_count: if image.volume { 1 } else { image.layers },
            },
            image_offset: vk::Offset3D::default(),
            image_extent: vk::Extent3D {
                width: l.width,
                height: l.height,
                depth: if image.volume { l.depth } else { 1 },
            },
        };
    }
    count
}

/// The copy and the barriers either side of it. The image's earlier contents are not
/// kept, but its earlier readers -- a submission still running, or the draws of this one
/// before it -- are waited for (its tracked use); it goes back to being sampled, which
/// draws cannot bring it to themselves.
///
/// # Safety
/// `command` recording on `vk`, `image` and `source` its.
unsafe fn record_copy(vk: &Vk, command: vk::CommandBuffer, image: &mut Image, source: vk::Buffer, offset: u64) {
    // SAFETY: the caller's.
    unsafe {
        if let Some(t) = image.track.enter(tracked::COPIED_TO, false) {
            util::renderer_transition(vk, command, image.image, vk::ImageAspectFlags::COLOR, &t);
        }
        let mut list = [vk::BufferImageCopy::default(); MAX_LEVELS];
        let count = regions(image, offset, &mut list);
        vk.cmd_copy_buffer_to_image(
            command,
            source,
            image.image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &list[..count],
        );
        if let Some(t) = image.track.enter(tracked::SAMPLED, true) {
            util::renderer_transition(vk, command, image.image, vk::ImageAspectFlags::COLOR, &t);
        }
    }
}

impl Dev {
    /// Whether a texture of these sizes is past what the device can make. A desynchronised
    /// command stream leaves fetch constants of impossible size, and creating one is
    /// undefined behaviour rather than an error. Each kind of image has its own limits
    /// (Xenia's GetMaxHostTextureWidthHeight and GetMaxHostTextureDepthOrArraySize): a
    /// volume's sides and depth, a cube's side (the cube make_image makes), and a 2D image's
    /// sides and layers.
    fn too_large(&self, header: &TextureHeader) -> bool {
        let (most_side, most_depth) = if header.volume {
            (self.max_3d, self.max_3d)
        } else if header.layers == 6 {
            (self.max_cube, 6)
        } else {
            (self.max_dimension, self.max_layers)
        };
        header.width > most_side || header.height > most_side || header.layers > most_depth
    }

    /// # Safety
    /// `vk` is this device's.
    unsafe fn make_buffer(
        &self,
        vk: &Vk,
        size: u64,
        usage: vk::BufferUsageFlags,
        want: vk::MemoryPropertyFlags,
    ) -> Option<(vk::Buffer, vk::DeviceMemory)> {
        let info = vk::BufferCreateInfo { size, usage, sharing_mode: vk::SharingMode::EXCLUSIVE, ..Default::default() };
        let mut buffer = vk::Buffer::null();
        // SAFETY: the caller's device; the buffer is destroyed on every way out but success.
        unsafe {
            if vk.create_buffer(&info, &mut buffer) != vk::Result::SUCCESS {
                return None;
            }
            let needs = vk.get_buffer_memory_requirements(buffer);
            let ty = util::find_memory(&self.memory, needs.memory_type_bits, want);
            if ty == u32::MAX {
                vk.destroy_buffer(buffer);
                return None;
            }
            let allocate =
                vk::MemoryAllocateInfo { allocation_size: needs.size, memory_type_index: ty, ..Default::default() };
            let mut memory = vk::DeviceMemory::null();
            if vk.allocate_memory(&allocate, &mut memory) != vk::Result::SUCCESS {
                vk.destroy_buffer(buffer);
                return None;
            }
            let _ = vk.bind_buffer_memory(buffer, memory, 0);
            Some((buffer, memory))
        }
    }

    /// # Safety
    /// `vk` is this device's.
    unsafe fn make_staging(&self, vk: &Vk, size: u64) -> Option<StagingBuffer> {
        // SAFETY: the caller's device.
        unsafe {
            let (buffer, memory) = self.make_buffer(
                vk,
                size,
                vk::BufferUsageFlags::TRANSFER_SRC,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )?;
            let mut mapped: *mut c_void = ptr::null_mut();
            if vk.map_memory(memory, 0, size, &mut mapped) != vk::Result::SUCCESS {
                vk.destroy_buffer(buffer);
                vk.free_memory(memory);
                return None;
            }
            Some(StagingBuffer { buffer, memory, mapped: mapped.cast(), size })
        }
    }

    /// # Safety
    /// `vk` is this device's, nothing pending reads the buffer.
    unsafe fn free_staging(&self, vk: &Vk, staging: &mut StagingBuffer) {
        // SAFETY: the caller's.
        unsafe {
            if staging.buffer != vk::Buffer::null() {
                vk.destroy_buffer(staging.buffer);
            }
            if staging.memory != vk::DeviceMemory::null() {
                vk.free_memory(staging.memory);
            }
        }
        *staging = StagingBuffer::default();
    }

    /// The image, its memory and its view, for `out` as its fields describe it.
    ///
    /// # Safety
    /// `vk` is this device's.
    unsafe fn make_image(
        &self,
        vk: &Vk,
        memory: &mut ImageMemory,
        fails: &mut Failures,
        out: &mut Image,
        mut error: Option<&mut &'static CStr>,
    ) -> bool {
        // Made by hand rather than read from a fetch constant: all of it is the one level.
        if out.levels.is_empty() {
            out.levels.push(Level {
                width: out.width,
                height: out.height,
                depth: if out.volume { out.layers } else { 1 },
                blocks_wide: out.blocks_wide,
                blocks_high: out.blocks_high,
                layers: out.layers,
                offset: 0,
                size: out.bytes as usize,
            });
        }
        // SAFETY: the caller's device, its instance's physical device.
        let properties = unsafe { vk.get_physical_device_format_properties(self.physical, out.format) };
        // A driver with no BC support would sample garbage rather than refuse.
        if !properties.optimal_tiling_features.contains(vk::FormatFeatureFlags::SAMPLED_IMAGE) {
            return fails.fail(c"device cannot sample this format", error);
        }

        let cube = !out.volume && out.layers == 6;
        if cube && out.width != out.height {
            return fails.fail(c"a cube map that is not square", error);
        }

        let info = vk::ImageCreateInfo {
            flags: if cube { vk::ImageCreateFlags::CUBE_COMPATIBLE } else { vk::ImageCreateFlags::empty() },
            image_type: if out.volume { vk::ImageType::TYPE_3D } else { vk::ImageType::TYPE_2D },
            format: out.format,
            extent: vk::Extent3D {
                width: out.width,
                height: out.height,
                depth: if out.volume { out.layers } else { 1 },
            },
            mip_levels: out.levels.len().max(1) as u32,
            array_layers: if out.volume { 1 } else { out.layers },
            samples: vk::SampleCountFlags::TYPE_1,
            tiling: vk::ImageTiling::OPTIMAL,
            // TRANSFER_SRC only so an upload can be read back and compared.
            usage: vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC,
            sharing_mode: vk::SharingMode::EXCLUSIVE,
            initial_layout: vk::ImageLayout::UNDEFINED,
            ..Default::default()
        };
        // SAFETY: the caller's device; what is made here is out's, destroyed with it.
        unsafe {
            let r = vk.create_image(&info, &mut out.image);
            out.track = TrackedImage::NEW;
            if r != vk::Result::SUCCESS {
                return fails.fail(explain(r), error.take());
            }
            let needs = vk.get_image_memory_requirements(out.image);
            let ty = util::find_memory(&self.memory, needs.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL);
            if ty == u32::MAX {
                return fails.fail(c"no device-local memory", error);
            }
            out.memory = memory.allocate(vk, &needs, ty);
            if !out.memory.is_some() {
                return fails.fail(explain(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY), error);
            }
            let _ = vk.bind_image_memory(out.image, out.memory.memory, out.memory.offset);

            // The fetch constant's swizzle, read through the host format's own (upload_one),
            // maps straight onto Vulkan's.
            let component = |selector: u32| match selector & 7 {
                0 => vk::ComponentSwizzle::R,
                1 => vk::ComponentSwizzle::G,
                2 => vk::ComponentSwizzle::B,
                3 => vk::ComponentSwizzle::A,
                4 => vk::ComponentSwizzle::ZERO,
                5 => vk::ComponentSwizzle::ONE,
                _ => vk::ComponentSwizzle::IDENTITY,
            };
            let view = vk::ImageViewCreateInfo {
                image: out.image,
                view_type: if out.volume {
                    vk::ImageViewType::TYPE_3D
                } else if cube {
                    vk::ImageViewType::CUBE
                } else {
                    vk::ImageViewType::TYPE_2D
                },
                format: out.format,
                components: vk::ComponentMapping {
                    r: component(out.swizzle),
                    g: component(out.swizzle >> 3),
                    b: component(out.swizzle >> 6),
                    a: component(out.swizzle >> 9),
                },
                subresource_range: vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: vk::REMAINING_MIP_LEVELS,
                    base_array_layer: 0,
                    layer_count: if out.volume { 1 } else { out.layers },
                },
                ..Default::default()
            };
            let r = vk.create_image_view(&view, &mut out.view);
            if r != vk::Result::SUCCESS {
                return fails.fail(explain(r), error);
            }
        }
        true
    }

    /// # Safety
    /// `vk` is this device's; nothing pending reads the image.
    unsafe fn destroy_image(&self, vk: &Vk, memory: &mut ImageMemory, image: &mut Image) {
        if image.borrowed {
            *image = Image::default();
            return;
        }
        // SAFETY: the caller's.
        unsafe {
            if image.view != vk::ImageView::null() {
                vk.destroy_image_view(image.view);
            }
            if image.image != vk::Image::null() {
                vk.destroy_image(image.image);
            }
            memory.free(vk, &mut image.memory);
        }
        *image = Image::default();
    }
}

impl Uploads {
    /// An upload outside a submission's segments, and a read-back: each records, submits
    /// and waits on its own; after everything the frames before it have queued.
    ///
    /// # Safety
    /// `vk` is the cache's device.
    unsafe fn begin_commands(&self, vk: &Vk) -> vk::CommandBuffer {
        outside::drain();
        // SAFETY: the caller's; the pool is this thread's, and the recorder has made every
        // call that could reach its buffer.
        unsafe {
            let _ = vk.reset_command_pool(self.commands, vk::CommandPoolResetFlags::empty());
            let begin = vk::CommandBufferBeginInfo {
                flags: vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT,
                ..Default::default()
            };
            if vk.begin_command_buffer(self.command, &begin) != vk::Result::SUCCESS {
                return vk::CommandBuffer::null();
            }
        }
        self.command
    }

    /// # Safety
    /// As begin_commands', after it.
    unsafe fn end_commands(&self, vk: &Vk, dev: &Dev) -> bool {
        // SAFETY: the caller's; the queue is held for the reset and the submission.
        unsafe {
            if vk.end_command_buffer(self.command) != vk::Result::SUCCESS {
                return false;
            }
            let commands = [self.command];
            let submit =
                vk::SubmitInfo { command_buffer_count: 1, p_command_buffers: commands.as_ptr(), ..Default::default() };
            {
                let _queue = super::QueueLock::hold();
                let _ = vk.reset_fences(&[self.fence]);
                if super::failed(
                    vk.queue_submit(dev.queue, std::slice::from_ref(&submit), self.fence),
                    c"a texture upload",
                ) {
                    return false;
                }
            }
            !super::failed(vk.wait_for_fences(&[self.fence], true, u64::MAX), c"a texture upload's fence")
        }
    }

    /// # Safety
    /// `vk` is the cache's device, `slot` open, `image` made on it, `bytes` readable for
    /// `size`.
    #[allow(clippy::too_many_arguments)]
    unsafe fn record_upload(
        &mut self,
        vk: &Vk,
        dev: &Dev,
        fails: &mut Failures,
        slot: usize,
        image: &mut Image,
        bytes: *const u8,
        size: usize,
        error: Option<&mut &'static CStr>,
    ) -> bool {
        // SAFETY: the caller's device; the slot is open.
        let Some((into, offset, command)) = (unsafe { self.staging_for(vk, dev, fails, slot, size) }) else {
            if let Some(error) = error {
                *error = fails.last_error.unwrap_or(c"no staging buffer");
            }
            return false;
        };
        // SAFETY: `into` is mapped for at least offset + size (a fresh buffer of `size`, or
        // the slot's, which holds at + size); the caller's bytes. Written, never read back.
        unsafe {
            ptr::copy_nonoverlapping(bytes, into.mapped.add(offset as usize), size);
            record_copy(vk, command, image, into.buffer, offset);
        }
        self.batched = self.batched.wrapping_add(1);
        true
    }

    /// Room for `size` bytes in the open slot's staging (or a buffer of its own when it
    /// overflows), and the segment's command buffer, begun: the buffer, the offset in it
    /// and the command buffer.
    ///
    /// # Safety
    /// `vk` is the cache's device, `slot` open.
    unsafe fn staging_for(
        &mut self,
        vk: &Vk,
        dev: &Dev,
        fails: &mut Failures,
        slot: usize,
        size: usize,
    ) -> Option<(StagingBuffer, u64, vk::CommandBuffer)> {
        let s = &mut self.slots[slot];
        // SAFETY: the caller's device; the slot's pool is this thread's alone.
        unsafe {
            if !s.recording {
                // A segment's buffer is made the first time a submission has that many.
                if s.segment >= s.commands.len() {
                    let allocate = vk::CommandBufferAllocateInfo {
                        command_pool: s.pool,
                        level: vk::CommandBufferLevel::PRIMARY,
                        command_buffer_count: 1,
                        ..Default::default()
                    };
                    let mut made = [vk::CommandBuffer::null()];
                    if vk.allocate_command_buffers(&allocate, &mut made) != vk::Result::SUCCESS {
                        fails.fail(c"no upload command buffer", None);
                        return None;
                    }
                    s.commands.push(made[0]);
                }
                let begin = vk::CommandBufferBeginInfo {
                    flags: vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT,
                    ..Default::default()
                };
                if vk.begin_command_buffer(s.commands[s.segment], &begin) != vk::Result::SUCCESS {
                    fails.fail(c"no upload command buffer", None);
                    return None;
                }
                s.recording = true;
            }
        }
        // Offsets a multiple of sixteen suit every block size uploaded.
        let at = s.used.wrapping_add(15) & !15u64;
        s.wanted = (s.wanted.wrapping_add(15) & !15u64).wrapping_add(size as u64);
        let (into, offset) = if s.staging.buffer == vk::Buffer::null() || at.wrapping_add(size as u64) > s.staging.size
        {
            self.overflows = self.overflows.wrapping_add(1);
            // SAFETY: the caller's device.
            let Some(made) = (unsafe { dev.make_staging(vk, size as u64) }) else {
                fails.fail(c"no staging buffer", None);
                return None;
            };
            s.overflow.push(made);
            (made, 0u64)
        } else {
            s.used = at.wrapping_add(size as u64);
            (s.staging, at)
        };
        Some((into, offset, s.commands[s.segment]))
    }

    /// Parts of the image staged and copied in, into the open slot's segment, over what the
    /// image holds: `bytes` the parts' bytes one after another, each region's buffer offset
    /// into them (texture_parts.rs). False, with nothing recorded, when no slot is open.
    ///
    /// # Safety
    /// `vk` is the cache's device, `image` made on it.
    unsafe fn stage_parts(
        &mut self,
        vk: &Vk,
        dev: &Dev,
        fails: &mut Failures,
        image: &mut Image,
        bytes: &[u8],
        regions: &mut [vk::BufferImageCopy],
    ) -> bool {
        let Some(slot) = self.open else { return false };
        let size = bytes.len();
        // SAFETY: the caller's device; the slot is open.
        let Some((into, offset, command)) = (unsafe { self.staging_for(vk, dev, fails, slot, size) }) else {
            return false;
        };
        for region in regions.iter_mut() {
            region.buffer_offset = region.buffer_offset.wrapping_add(offset);
        }
        image.staged = image.staged.wrapping_add(1);
        // SAFETY: `into` is mapped for at least offset + size; the image is the caller's.
        // Written, never read back.
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), into.mapped.add(offset as usize), size);
            // The rest of the image is kept: the transition keeps its contents.
            if let Some(t) = image.track.enter(tracked::COPIED_TO, true) {
                util::renderer_transition(vk, command, image.image, vk::ImageAspectFlags::COLOR, &t);
            }
            vk.cmd_copy_buffer_to_image(command, into.buffer, image.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, regions);
            if let Some(t) = image.track.enter(tracked::SAMPLED, true) {
                util::renderer_transition(vk, command, image.image, vk::ImageAspectFlags::COLOR, &t);
            }
        }
        self.batched = self.batched.wrapping_add(1);
        true
    }

    /// The image's bytes staged and copied in: into the open slot's segment, or alone.
    ///
    /// # Safety
    /// `vk` is the cache's device, `image` made on it, `bytes` readable for `size`.
    #[allow(clippy::too_many_arguments)]
    unsafe fn stage_into(
        &mut self,
        vk: &Vk,
        dev: &Dev,
        fails: &mut Failures,
        image: &mut Image,
        bytes: *const u8,
        size: usize,
        error: Option<&mut &'static CStr>,
    ) -> bool {
        image.staged = image.staged.wrapping_add(1);
        if let Some(open) = self.open {
            // MW2_FORCE_LONE_UPLOADS: each submitted on its own, as before the frame's uploads.
            #[cfg(feature = "diagnostics")]
            let lone = super::trace::forcing().lone_uploads;
            #[cfg(not(feature = "diagnostics"))]
            let lone = false;
            if !lone {
                // SAFETY: the caller's.
                return unsafe { self.record_upload(vk, dev, fails, open, image, bytes, size, error) };
            }
            #[cfg(feature = "diagnostics")]
            {
                let counts = super::trace::counts();
                counts.lone_uploads.store(
                    counts.lone_uploads.load(std::sync::atomic::Ordering::Relaxed).wrapping_add(1),
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
        }

        // SAFETY: the caller's device; the staging lives until the copy has run (the fence).
        unsafe {
            let Some(mut staging) = dev.make_staging(vk, size as u64) else {
                return fails.fail(c"no staging buffer", error);
            };
            ptr::copy_nonoverlapping(bytes, staging.mapped, size);
            let mut ok = false;
            let command = self.begin_commands(vk);
            if command != vk::CommandBuffer::null() {
                record_copy(vk, command, image, staging.buffer, 0);
                ok = self.end_commands(vk, dev);
            }
            dev.free_staging(vk, &mut staging);
            self.alone = self.alone.wrapping_add(1);
            if !ok {
                return fails.fail(c"upload submission failed", error);
            }
        }
        true
    }
}

impl Bindless {
    /// The bindless set's entry at `element`: the image's view.
    ///
    /// # Safety
    /// `vk` is the cache's device, the view its, and nothing pending reads the entry.
    unsafe fn write_image(&mut self, vk: &Vk, element: u32, image: &mut Image) {
        let info = [vk::DescriptorImageInfo {
            sampler: vk::Sampler::null(),
            image_view: image.view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let write = vk::WriteDescriptorSet {
            dst_set: self.set,
            dst_binding: bindings::IMAGES,
            dst_array_element: element,
            descriptor_count: 1,
            descriptor_type: vk::DescriptorType::SAMPLED_IMAGE,
            p_image_info: info.as_ptr(),
            ..Default::default()
        };
        // SAFETY: the caller's.
        unsafe { vk.update_descriptor_sets(std::slice::from_ref(&write), &[]) };
        image.written = image.view;
        self.written = self.written.wrapping_add(1);
    }
}

impl TextureCache {
    pub fn new() -> TextureCache {
        TextureCache::default()
    }

    fn find(&self, id: u64) -> Option<&Image> {
        let id = id & IMAGE_BITS;
        if id == 0 || id > self.images.len() as u64 {
            return None;
        }
        Some(&self.images[(id - 1) as usize])
    }

    fn find_mut(&mut self, id: u64) -> Option<&mut Image> {
        let id = id & IMAGE_BITS;
        if id == 0 || id > self.images.len() as u64 {
            return None;
        }
        Some(&mut self.images[(id - 1) as usize])
    }

    /// One sampler per distinct state; its slot in the bindless set is written as it is
    /// made, and a sampler is never destroyed before the cache is, so the slot holds it for
    /// good. None when it could not be made.
    fn sampler_for(&mut self, vk: &Vk, key: u64) -> Sampler {
        if let Some(&found) = self.samplers.get(&key) {
            return found;
        }
        let slot = self.samplers.len() as u32;
        if slot >= self.dev.info.samplers {
            return Sampler::default();
        }
        let mip = (key >> 4) & 3;
        let mut info = vk::SamplerCreateInfo {
            mag_filter: filter_of((key & 3) as u32),
            min_filter: filter_of(((key >> 2) & 3) as u32),
            mipmap_mode: if mip == 1 { vk::SamplerMipmapMode::LINEAR } else { vk::SamplerMipmapMode::NEAREST },
            address_mode_u: address_mode(((key >> 6) & 7) as u32),
            address_mode_v: address_mode(((key >> 9) & 7) as u32),
            address_mode_w: address_mode(((key >> 12) & 7) as u32),
            ..Default::default()
        };
        // A cube's faces clamp at their edges, each filtered on its own as Direct3D 9
        // filters them, where the device allows it.
        if (key >> 24) & 1 != 0 {
            info.address_mode_u = vk::SamplerAddressMode::CLAMP_TO_EDGE;
            info.address_mode_v = vk::SamplerAddressMode::CLAMP_TO_EDGE;
            info.address_mode_w = vk::SamplerAddressMode::CLAMP_TO_EDGE;
        }
        if self.dev.info.non_seamless_cubes {
            info.flags |= vk::SamplerCreateFlags::NON_SEAMLESS_CUBE_MAP_EXT;
        }
        info.border_color = if (key >> 15) & 3 == 1 {
            vk::BorderColor::FLOAT_OPAQUE_WHITE
        } else {
            vk::BorderColor::FLOAT_TRANSPARENT_BLACK
        };
        // Only the lowest end needs a clamp; BaseMap a quarter above it (Xenia, BSD-3).
        info.min_lod = ((key >> 17) & 15) as f32;
        info.max_lod = if mip == 2 { info.min_lod + 0.25 } else { vk::LOD_CLAMP_NONE };
        // 1:1 is no anisotropy; 2:1 and up double each step.
        let aniso = ((key >> 21) & 7) as u32;
        let most = self.dev.info.max_anisotropy;
        if (2..=5).contains(&aniso) && most > 1.0 {
            info.anisotropy_enable = vk::TRUE;
            let wanted = (1u32 << (aniso - 1)) as f32;
            info.max_anisotropy = if most < wanted { most } else { wanted };
        }

        let mut sampler = vk::Sampler::null();
        // SAFETY: the cache's device; the slot is free (each sampler gets the next one).
        unsafe {
            if vk.create_sampler(&info, &mut sampler) != vk::Result::SUCCESS {
                return Sampler::default();
            }
            let image = [vk::DescriptorImageInfo {
                sampler,
                image_view: vk::ImageView::null(),
                image_layout: vk::ImageLayout::UNDEFINED,
            }];
            let write = vk::WriteDescriptorSet {
                dst_set: self.bind.set,
                dst_binding: bindings::SAMPLERS,
                dst_array_element: slot,
                descriptor_count: 1,
                descriptor_type: vk::DescriptorType::SAMPLER,
                p_image_info: image.as_ptr(),
                ..Default::default()
            };
            vk.update_descriptor_sets(std::slice::from_ref(&write), &[]);
        }
        let made = Sampler { sampler, slot };
        self.samplers.insert(key, made);
        made
    }

    /// The sampler's place in an id, making it on first sight; zero when it could not be
    /// made.
    fn sampler_index_for(&mut self, vk: &Vk, key: u64) -> u32 {
        if let Some(&found) = self.sampler_index.get(&key) {
            return found;
        }
        let sampler = self.sampler_for(vk, key);
        if sampler.sampler == vk::Sampler::null() {
            return 0;
        }
        self.sampler_list.push(sampler);
        let index = self.sampler_list.len() as u32;
        self.sampler_index.insert(key, index);
        index
    }

    /// The stand-in of each kind, at its kind's element of set 1's image array, for a slot
    /// with nothing bound and one bound to a texture of another kind than the module
    /// declares.
    fn make_fallback_of(&mut self, vk: &Vk, which: u8, layers: u32, volume: bool) -> bool {
        let mut out = Image {
            format: vk::Format::R8G8B8A8_UNORM,
            width: 1,
            height: 1,
            blocks_wide: 1,
            blocks_high: 1,
            layers,
            volume,
            bytes: 4 * u64::from(layers),
            ..Image::default()
        };
        let ok = 'made: {
            // SAFETY: the cache's device.
            if !unsafe { self.dev.make_image(vk, &mut self.memory, &mut self.fails, &mut out, None) } {
                break 'made false;
            }
            let texels = vec![0u8; 4 * layers as usize];
            // SAFETY: as above; the texels live through the call.
            if !unsafe {
                self.up.stage_into(vk, &self.dev, &mut self.fails, &mut out, texels.as_ptr(), texels.len(), None)
            } {
                break 'made false;
            }
            let sampler = self.sampler_for(vk, 0);
            out.sampler = sampler.sampler;
            out.sampler_slot = sampler.slot;
            if out.sampler == vk::Sampler::null() {
                break 'made false;
            }
            // SAFETY: as above; the stand-in's element is written once.
            unsafe { self.bind.write_image(vk, u32::from(kind_of(&out)), &mut out) };
            true
        };
        // What was made is the stand-in's either way, destroyed with it at shutdown.
        match which {
            0 => self.fallback = out,
            1 => self.fallback_cube = out,
            _ => self.fallback_3d = out,
        }
        ok
    }

    fn make_fallback(&mut self, vk: &Vk) -> bool {
        self.make_fallback_of(vk, 0, 1, false)
            && self.make_fallback_of(vk, 1, 6, false)
            && self.make_fallback_of(vk, 2, 1, true)
    }

    fn fallback_of(&self, kind: u8) -> &Image {
        match kind {
            2 => &self.fallback_3d,
            1 => &self.fallback_cube,
            _ => &self.fallback,
        }
    }

    /// A slot of `images`, one-based: zero means nothing. A released image's slot is used
    /// again; nothing still holds its id by then (release).
    fn new_slot(&mut self) -> u64 {
        if let Some(id) = self.free_ids.pop() {
            return id;
        }
        // An id also places the image in the bindless set, which has room for so many.
        if element_of(self.images.len() as u64 + 1) >= bindings::IMAGE_COUNT {
            return 0;
        }
        self.images.push(Image::default());
        self.images.len() as u64
    }

    /// `id` from new_slot.
    fn store(&mut self, vk: &Vk, id: u64, mut image: Image) {
        image.last_bound = self.track.frame;
        image.bound_segment = self.track.segment;
        image.last_submission = self.track.submission;
        image.last_used_at = self.track.submission_began;
        self.uploads = self.uploads.wrapping_add(1);
        self.uploaded_bytes = self.uploaded_bytes.wrapping_add(image.bytes);
        self.live_bytes = self.live_bytes.wrapping_add(image.bytes);
        let stored = &mut self.images[(id - 1) as usize];
        *stored = image;
        // SAFETY: the cache's device; a new id's entry is read by nothing pending.
        unsafe { self.bind.write_image(vk, element_of(id), stored) };
    }

    /// The bindless slot of the sampler an id is bound with.
    fn sampler_slot_of(&self, id: u64, image: &Image) -> u32 {
        let index = ((id >> SAMPLER_SHIFT) as u32) & 0x3FFF_FFFF;
        if index != 0 && index as usize <= self.sampler_list.len() {
            self.sampler_list[(index - 1) as usize].slot
        } else {
            image.sampler_slot
        }
    }

    /// `signature` and `source_bytes` describe the guest memory the texture was read from,
    /// for one that came from a fetch constant.
    #[allow(clippy::too_many_arguments)]
    fn upload_one(
        &mut self,
        vk: &Vk,
        data: &TextureData,
        xenos_format: u32,
        sampler_key: u64,
        swizzle: u32,
        mut error: Option<&mut &'static CStr>,
        signature: u64,
        source_bytes: usize,
        linearise: bool,
    ) -> u64 {
        if self.dev.device == vk::Device::null() {
            if let Some(error) = error {
                *error = c"no device";
            }
            return NONE;
        }

        let direct = formats::vulkan_format_for(xenos_format);
        if direct == vk::Format::UNDEFINED.as_raw() {
            self.fails.fail(c"no Vulkan equivalent for the format", error);
            return NONE;
        }
        let physical = self.dev.physical;
        // SAFETY: the cache's device's physical device.
        let features = |format: vk::Format| unsafe { vk.format_features(physical, format) };
        let (format, convert, srgb_refused) = host_format(direct, linearise, features);
        if srgb_refused {
            self.linearised_refused = self.linearised_refused.wrapping_add(1);
        }
        let filterable = format_support::filterable(features(vk::Format::from_raw(format)));
        let info = texture::texture_format_of(xenos_format);

        let header = &data.header;
        if self.dev.too_large(header) {
            self.fails.fail(c"larger than the device allows", error);
            return NONE;
        }
        // The bytes in the host format's layout, when they are not already.
        let converted;
        let data = if convert != Convert::None {
            // Data made by hand names no levels: all of it is the one (make_image's).
            let whole = [Level {
                width: header.width,
                height: header.height,
                depth: if header.volume { header.layers } else { 1 },
                blocks_wide: header.blocks_wide,
                blocks_high: header.blocks_high,
                layers: header.layers,
                offset: 0,
                size: data.bytes.len(),
            }];
            let levels_in = if data.levels().is_empty() { &whole[..] } else { data.levels() };
            let mut levels = Vec::new();
            let bytes = texture_convert::convert(convert, levels_in, &data.bytes, &mut levels);
            let mut header = data.header;
            header.level_count = levels.len() as u32;
            header.levels[..levels.len()].copy_from_slice(&levels);
            if convert.expands() {
                header.blocks_wide = header.blocks_wide.wrapping_mul(4);
                header.blocks_high = header.blocks_high.wrapping_mul(4);
                header.bytes_per_block = convert.texel_bytes();
                header.expanded = true;
            }
            converted = TextureData { header, bytes };
            &converted
        } else {
            data
        };
        let header = &data.header;

        let mut image = Image {
            layers: header.layers,
            volume: header.volume,
            source_bytes,
            signature,
            // A guest format with fewer channels than the host format carrying it repeats
            // its last.
            swizzle: formats::compose_swizzle(swizzle, formats::host_swizzle_for(xenos_format)),
            format: vk::Format::from_raw(format),
            width: header.width,
            height: header.height,
            blocks_wide: header.blocks_wide,
            blocks_high: header.blocks_high,
            block_width: if header.expanded { 1 } else { info.block_width },
            block_height: if header.expanded { 1 } else { info.block_height },
            bytes: data.bytes.len() as u64,
            levels: data.levels().to_vec(),
            convert,
            filterable,
            ..Image::default()
        };
        if convert != Convert::None {
            self.converted = self.converted.wrapping_add(1);
        }
        if !filterable {
            self.point_only = self.point_only.wrapping_add(1);
        }

        // The sampler and the id before the image: once its upload is recorded, the image
        // may not be destroyed until that has run.
        let sampler = self.sampler_for(vk, point_only(sampler_key, filterable));
        image.sampler = sampler.sampler;
        image.sampler_slot = sampler.slot;
        if image.sampler == vk::Sampler::null() {
            self.fails.fail(c"no sampler", error);
            return NONE;
        }
        let id = self.new_slot();
        if id == 0 {
            self.fails.fail(c"no room in the bindless set", error);
            return NONE;
        }
        // SAFETY: the cache's device; the bytes live through the call.
        let made = unsafe {
            self.dev.make_image(vk, &mut self.memory, &mut self.fails, &mut image, error.as_deref_mut())
                && self.up.stage_into(
                    vk,
                    &self.dev,
                    &mut self.fails,
                    &mut image,
                    data.bytes.as_ptr(),
                    data.bytes.len(),
                    error,
                )
        };
        if !made {
            // SAFETY: as above; nothing was recorded that reads the image (a recorded
            // upload succeeded and so `made` would hold).
            unsafe { self.dev.destroy_image(vk, &mut self.memory, &mut image) };
            self.free_ids.push(id);
            return NONE;
        }
        self.store(vk, id, image);
        id
    }

    // ---- start and end ----

    /// Starts the cache on the pipeline's device (which it starts if it is not running).
    /// Safe to call repeatedly.
    pub fn initialise(&mut self, vk: &Vk) -> bool {
        if self.dev.device != vk::Device::null() {
            return true;
        }
        let Some(info) = textures_device() else {
            return false;
        };
        self.dev.device = vk::Device::from_raw(info.device);
        self.dev.physical = vk::PhysicalDevice::from_raw(info.physical);
        self.dev.queue = vk::Queue::from_raw(info.queue);
        self.dev.info = info;
        if self.dev.device == vk::Device::null() || self.dev.queue == vk::Queue::null() {
            log_warn!("textures: the pipeline device has no queue to upload on");
            self.dev.device = vk::Device::null();
            return false;
        }
        // SAFETY: the pipeline's device and its physical device.
        unsafe {
            self.dev.memory = vk.get_physical_device_memory_properties(self.dev.physical);
            let properties = vk.get_physical_device_properties(self.dev.physical);
            self.dev.max_dimension = properties.limits.max_image_dimension2_d;
            self.dev.max_cube = properties.limits.max_image_dimension_cube;
            self.dev.max_3d = properties.limits.max_image_dimension3_d;
            self.dev.max_layers = properties.limits.max_image_array_layers;
        }
        // Half the device's own memory, within reason, unless MW2_TEXTURE_BUDGET_MB says
        // otherwise.
        let mut device_local = 0u64;
        let heaps =
            &self.dev.memory.memory_heaps[..(self.dev.memory.memory_heap_count as usize).min(vk::MAX_MEMORY_HEAPS)];
        for heap in heaps {
            if heap.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL) {
                device_local = device_local.max(heap.size);
            }
        }
        self.budget = crate::env::number(c"MW2_TEXTURE_BUDGET_MB", 0) << 20;
        #[cfg(feature = "diagnostics")]
        {
            let least = super::trace::forcing().staging_least_kb;
            if least != 0 {
                self.up.staging_least = (u64::from(least) << 10).min(STAGING_MOST);
            }
        }
        if self.budget == 0 {
            // std::clamp's answer for integers.
            self.budget = (device_local / 2).clamp(256 << 20, 2048 << 20);
        }

        // SAFETY: the pipeline's device; what is made here is destroyed at shutdown.
        unsafe {
            let pool = vk::CommandPoolCreateInfo {
                flags: vk::CommandPoolCreateFlags::TRANSIENT,
                queue_family_index: self.dev.info.queue_family,
                ..Default::default()
            };
            if vk.create_command_pool(&pool, &mut self.up.commands) != vk::Result::SUCCESS {
                log_warn!("textures: no command pool");
                self.shutdown(vk);
                return false;
            }
            let allocate = vk::CommandBufferAllocateInfo {
                command_pool: self.up.commands,
                level: vk::CommandBufferLevel::PRIMARY,
                command_buffer_count: 1,
                ..Default::default()
            };
            let mut command = [vk::CommandBuffer::null()];
            if vk.allocate_command_buffers(&allocate, &mut command) != vk::Result::SUCCESS {
                log_warn!("textures: no command buffer");
                self.shutdown(vk);
                return false;
            }
            self.up.command = command[0];
            let fence = vk::FenceCreateInfo::default();
            if vk.create_fence(&fence, &mut self.up.fence) != vk::Result::SUCCESS {
                log_warn!("textures: no fence");
                self.shutdown(vk);
                return false;
            }
            for n in 0..UPLOAD_SLOTS as usize {
                if vk.create_command_pool(&pool, &mut self.up.slots[n].pool) != vk::Result::SUCCESS {
                    log_warn!("textures: no upload command buffers");
                    self.shutdown(vk);
                    return false;
                }
            }
        }

        if !self.make_bindless_set(vk) {
            log_warn!("textures: no descriptor set for the textures");
            self.shutdown(vk);
            return false;
        }

        if !self.make_fallback(vk) {
            let why = self.fails.last_error.unwrap_or(c"");
            log_warn!("textures: the placeholder image would not upload (%s)", why);
            self.shutdown(vk);
            return false;
        }

        log_info!("textures: uploading on %s", self.dev.info.name.as_c_str());
        true
    }

    /// The one set every draw samples through.
    fn make_bindless_set(&mut self, vk: &Vk) -> bool {
        let sizes = [
            vk::DescriptorPoolSize { ty: vk::DescriptorType::SAMPLED_IMAGE, descriptor_count: bindings::IMAGE_COUNT },
            vk::DescriptorPoolSize { ty: vk::DescriptorType::SAMPLER, descriptor_count: bindings::SAMPLER_COUNT },
        ];
        let info = vk::DescriptorPoolCreateInfo {
            flags: vk::DescriptorPoolCreateFlags::UPDATE_AFTER_BIND,
            max_sets: 1,
            pool_size_count: 2,
            p_pool_sizes: sizes.as_ptr(),
            ..Default::default()
        };
        // SAFETY: the cache's device; the pool is destroyed at shutdown.
        unsafe {
            if vk.create_descriptor_pool(&info, &mut self.bind.pool) != vk::Result::SUCCESS {
                return false;
            }
            let layout = [vk::DescriptorSetLayout::from_raw(self.dev.info.texture_set_layout)];
            let allocate = vk::DescriptorSetAllocateInfo {
                descriptor_pool: self.bind.pool,
                descriptor_set_count: 1,
                p_set_layouts: layout.as_ptr(),
                ..Default::default()
            };
            if layout[0] == vk::DescriptorSetLayout::null() {
                return false;
            }
            let mut set = [vk::DescriptorSet::null()];
            let made = vk.allocate_descriptor_sets(&allocate, &mut set) == vk::Result::SUCCESS;
            self.bind.set = set[0];
            made
        }
    }

    pub fn shutdown(&mut self, vk: &Vk) {
        if self.dev.device == vk::Device::null() {
            return;
        }
        // SAFETY: the cache's device, idle from here on; everything destroyed is its own.
        unsafe {
            let _ = vk.device_wait_idle();
            let mut doomed = std::mem::take(&mut self.doomed);
            for image in doomed.iter_mut() {
                self.dev.destroy_image(vk, &mut self.memory, image);
            }
            let mut images = std::mem::take(&mut self.images);
            for image in images.iter_mut() {
                self.dev.destroy_image(vk, &mut self.memory, image);
            }
            self.free_ids.clear();
            self.by_fetch.clear();
            self.by_content.clear();
            self.failed_keys.clear();
            self.retrying.clear();
            self.sampler_list.clear();
            self.sampler_index.clear();
            self.dev.destroy_image(vk, &mut self.memory, &mut self.fallback);
            self.dev.destroy_image(vk, &mut self.memory, &mut self.fallback_cube);
            self.dev.destroy_image(vk, &mut self.memory, &mut self.fallback_3d);
            for sampler in self.samplers.values() {
                vk.destroy_sampler(sampler.sampler);
            }
            self.samplers.clear();
            if self.bind.pool != vk::DescriptorPool::null() {
                vk.destroy_descriptor_pool(self.bind.pool);
            }
            self.bind.pool = vk::DescriptorPool::null();
            self.bind.set = vk::DescriptorSet::null();
            self.memory.shutdown(vk);
            self.up.open = None;
            for n in 0..UPLOAD_SLOTS as usize {
                let mut slot = std::mem::take(&mut self.up.slots[n]);
                self.dev.free_staging(vk, &mut slot.staging);
                for extra in slot.overflow.iter_mut() {
                    self.dev.free_staging(vk, extra);
                }
                if slot.pool != vk::CommandPool::null() {
                    vk.destroy_command_pool(slot.pool);
                }
            }
            if self.up.fence != vk::Fence::null() {
                vk.destroy_fence(self.up.fence);
            }
            if self.up.commands != vk::CommandPool::null() {
                vk.destroy_command_pool(self.up.commands);
            }
        }
        self.up.fence = vk::Fence::null();
        self.up.commands = vk::CommandPool::null();
        self.up.command = vk::CommandBuffer::null();
        self.dev.device = vk::Device::null();
        #[cfg(feature = "diagnostics")]
        super::trace::finish(); // the last traced call of the run was above
    }

    // ---- the frame, and the guest's writes ----

    /// The count new_frame keeps, which the texture log lines are numbered by.
    pub fn frame_number(&self) -> u64 {
        self.track.frame
    }

    /// Guest memory under these bytes has just changed (a physical range): any texture
    /// read from it is signed again at its next bind, even later in the same frame.
    pub fn memory_written(&mut self, physical: u32, size: u32) {
        // A linear walk: a few thousand images, a couple of copies a frame.
        let end = physical.wrapping_add(size);
        let overlaps = |address: u32, bytes: usize| {
            bytes != 0 && address < end && u64::from(address).wrapping_add(bytes as u64) > u64::from(physical)
        };
        self.track.epoch = self.track.epoch.wrapping_add(1);
        for image in self.images.iter_mut() {
            if image.image == vk::Image::null() || image.borrowed {
                continue;
            }
            if !overlaps(image.address, image.source_bytes) && !overlaps(image.mip_address, image.mip_bytes) {
                continue;
            }
            image.checked_frame = u64::MAX;
            image.sign_period = 1;
            self.track.invalidated_by_writes = self.track.invalidated_by_writes.wrapping_add(1);
        }
    }

    /// Moves on whenever binding the same fetch constant again could come out differently
    /// or have to look at the texture again: a new frame or segment, an image let go,
    /// memory_written.
    #[inline]
    pub fn epoch(&self) -> u64 {
        self.track.epoch
    }

    /// Called once per presented frame, between submissions: the late reads looked at,
    /// the frame and epoch moved on, the time the images' age is held to taken, the cache
    /// looked over and what it let go destroyed.
    pub fn new_frame(&mut self, vk: &Vk) {
        // The frame ending is the one after the late reads of the frame before: bound in
        // it, or not.
        let reads = std::mem::take(&mut self.track.late_reads);
        let mut kept = Vec::with_capacity(reads.len());
        for read in reads {
            if read.frame == self.track.frame {
                kept.push(read);
                continue;
            }
            let found = self
                .find(read.id)
                .map(|image| (image.last_bound, image.width, image.height, image.xenos_format, image.address));
            if let Some((last_bound, ..)) = found
                && last_bound > read.frame
            {
                self.track.late_reads_bound_again = self.track.late_reads_bound_again.wrapping_add(1);
                continue;
            }
            self.track.late_reads_on_last_use = self.track.late_reads_on_last_use.wrapping_add(1);
            if let Some((_, width, height, format, address)) = found
                && (self.track.late_reads_on_last_use <= 24 || flash_hunt())
            {
                log_warn!(
                    "textures:   re-read on its last use, %ux%u fmt %u at %08X, frame %llu: its memory changed after its draws were parsed, and it is not bound again",
                    width,
                    height,
                    format,
                    address,
                    read.frame
                );
            }
        }
        self.track.late_reads = kept;
        self.track.frame = self.track.frame.wrapping_add(1);
        self.track.epoch = self.track.epoch.wrapping_add(1);
        // What failed in the frame before is looked up again at its next bind (failed_keys).
        // The cache lets images go only below, at a frame's start, so nothing a failure waits
        // for is freed within a frame.
        self.retrying.clear();
        for key in self.failed_keys.drain(..) {
            if self.by_fetch.get(&key) == Some(&NONE) {
                self.by_fetch.remove(&key);
                self.retrying.insert(key);
            }
        }
        self.track.now = self.clock();
        if self.dev.device == vk::Device::null() {
            return;
        }
        // Every half minute, what the cache holds and how much of it is in use.
        if cfg!(feature = "diagnostics") && self.track.frame.is_multiple_of(1800) {
            let (mut held, mut recent, mut recent_bytes) = (0u64, 0u64, 0u64);
            for image in &self.images {
                if image.image == vk::Image::null() || image.borrowed {
                    continue;
                }
                held += 1;
                if !self.aged(image) {
                    recent += 1;
                    recent_bytes = recent_bytes.wrapping_add(image.bytes);
                }
            }
            let memory = self.memory.held();
            log_info!(
                "textures: %llu held (%llu MB), %llu of them bound in the last 10 s (%llu MB); %llu blocks of memory (%llu MB), %llu ids",
                held,
                self.live_bytes >> 20,
                recent,
                recent_bytes >> 20,
                memory.blocks,
                memory.block_bytes >> 20,
                self.images.len() as u64
            );
        }
        if self.track.frame.is_multiple_of(SWEEP_FRAMES) && self.live_bytes > self.budget {
            self.evict();
        }
        self.sweep_images();
        self.destroy_doomed(vk);
    }

    /// Whether an image can be let go: one of the cache's own, unbound that long, and the
    /// submission it was last bound in finished on the GPU.
    fn idle(&self, image: &Image) -> bool {
        image.image != vk::Image::null()
            && !image.borrowed
            && !image.pinned
            && self.aged(image)
            && image.last_submission <= self.track.completed
    }

    /// Xenia's last_usage_time + lifetime > current_time keeps it.
    fn aged(&self, image: &Image) -> bool {
        image.last_used_at.saturating_add(UNUSED_NANOSECONDS) <= self.track.now
    }

    /// An idle image `id` is let go: counted, with how long and how many frames after its
    /// last use (by the rule never under UNUSED_NANOSECONDS, whatever the frame rate).
    #[cfg(feature = "diagnostics")]
    fn note_let_go(&mut self, id: u64) {
        let image = &self.images[(id - 1) as usize];
        let nanoseconds = self.track.now.saturating_sub(image.last_used_at);
        let frames = self.track.frame.wrapping_sub(image.last_bound);
        let l = &mut self.let_go;
        if l.count == 0 || nanoseconds < l.nanoseconds {
            l.nanoseconds = nanoseconds;
        }
        if l.count == 0 || frames < l.frames {
            l.frames = frames;
        }
        l.count += 1;
    }

    /// The images' clock: the host's, or MW2_DETERMINISTIC's frame count.
    fn clock(&self) -> u64 {
        #[cfg(feature = "diagnostics")]
        if super::trace::deterministic() {
            return self.track.frame.wrapping_mul(DETERMINISTIC_FRAME_NANOSECONDS);
        }
        outside::now()
    }

    /// The image stops existing for the cache now; its Vulkan objects are destroyed over
    /// the next frames (destroy_doomed).
    fn release(&mut self, id: u64) {
        let image = std::mem::take(&mut self.images[(id - 1) as usize]);
        self.track.epoch = self.track.epoch.wrapping_add(1);
        for name in &image.names {
            let key = FetchWords(*name);
            if let Some(&entry) = self.by_fetch.get(&key)
                && entry & IMAGE_BITS == id
            {
                self.by_fetch.remove(&key);
            }
        }
        let content = FetchWords(image.content);
        if self.by_content.get(&content) == Some(&id) {
            self.by_content.remove(&content);
        }
        self.live_bytes = self.live_bytes.wrapping_sub(image.bytes);
        self.doomed.push_back(image);
        self.free_ids.push(id);
    }

    /// A few a frame: destroying one is cheap now that its memory is a range of a block,
    /// but a sweep can let thousands go at once.
    fn destroy_doomed(&mut self, vk: &Vk) {
        let from = outside::now();
        while let Some(mut image) = self.doomed.pop_front() {
            // SAFETY: the cache's device; the frame that last named the image is finished on
            // the GPU, so made by the recorder (idle).
            unsafe { self.dev.destroy_image(vk, &mut self.memory, &mut image) };
            #[cfg(feature = "diagnostics")]
            if super::trace::deterministic() {
                continue; // every one due, whatever the time
            }
            if outside::now().wrapping_sub(from) >= DESTROY_NANOSECONDS {
                break;
            }
        }
    }

    /// An idle image whose memory has been written since it was read is released: the
    /// console holds no copies, and a level that streams keeps reusing the same memory. The
    /// write watch says which may have been, and its signature whether it was.
    fn sweep_images(&mut self) {
        let phase = self.track.frame % SWEEP_FRAMES;
        let count = self.images.len() as u64;
        let from = count.wrapping_mul(phase) / SWEEP_FRAMES;
        let to = count.wrapping_mul(phase + 1) / SWEEP_FRAMES;
        for i in from..to {
            let i = i as usize;
            let image = &self.images[i];
            if !self.idle(image) || image.watch_stamp == 0 || unchanged(image) {
                continue;
            }
            // The watch answers for whole pages, and a neighbour's write is not this
            // texture's: the bytes decide.
            #[cfg(feature = "diagnostics")]
            note_image(image);
            let stamp = track_image(self.track.frame, image);
            if sign_image(image, None) == image.signature {
                self.images[i].watch_stamp = stamp;
                self.stale_not_really = self.stale_not_really.wrapping_add(1);
                continue;
            }
            self.stale = self.stale.wrapping_add(1);
            self.stale_bytes = self.stale_bytes.wrapping_add(image.bytes);
            #[cfg(feature = "diagnostics")]
            self.note_changed(i);
            #[cfg(feature = "diagnostics")]
            self.note_let_go(i as u64 + 1);
            self.release(i as u64 + 1);
        }
    }

    /// Past the budget, the idle images bound longest ago go, down to three quarters of it.
    fn evict(&mut self) {
        let mut oldest: Vec<(u64, u64)> = Vec::new(); // last bound, id
        for (i, image) in self.images.iter().enumerate() {
            if self.idle(image) {
                oldest.push((image.last_bound, i as u64 + 1));
            }
        }
        if oldest.is_empty() {
            return;
        }
        oldest.sort_unstable();
        let target = self.budget / 4 * 3;
        let (mut count, mut bytes) = (0u64, 0u64);
        for (_, id) in oldest {
            if self.live_bytes <= target {
                break;
            }
            bytes = bytes.wrapping_add(self.images[(id - 1) as usize].bytes);
            count += 1;
            #[cfg(feature = "diagnostics")]
            self.note_let_go(id);
            self.release(id);
        }
        self.evicted = self.evicted.wrapping_add(count);
        self.evicted_bytes = self.evicted_bytes.wrapping_add(bytes);
        let passes = self.eviction_passes;
        self.eviction_passes = passes.wrapping_add(1);
        if passes < 4 {
            log_info!(
                "textures: over the %llu MB budget; evicted %llu (%llu MB) not bound for ten seconds",
                self.budget >> 20,
                count,
                bytes >> 20
            );
        }
    }

    // ---- uploads ----

    /// Opens upload slot `index` once the renderer knows the GPU is done with what that
    /// slot last held.
    /// A submission begins (renderer_frame.rs begin_frame): `serial` is the one it will be
    /// given, `completed` the last the GPU has finished, just looked at -- Xenia's
    /// CompletedSubmissionUpdated and BeginSubmission, for the images' age -- and upload
    /// slot `index`'s staging made ready.
    pub fn begin_uploads(&mut self, vk: &Vk, index: u32, serial: u64, completed: u64) {
        self.track.completed = completed;
        self.track.submission = serial;
        self.track.submission_began = self.clock();
        if self.dev.device == vk::Device::null() || index >= UPLOAD_SLOTS {
            return;
        }
        let n = index as usize;
        let least = self.up.staging_least;
        let dev = &self.dev;
        let slot = &mut self.up.slots[n];
        // SAFETY: the cache's device; the GPU is done with everything the slot held.
        unsafe {
            for extra in slot.overflow.iter_mut() {
                dev.free_staging(vk, extra);
            }
            let overflowed = !slot.overflow.is_empty();
            slot.overflow.clear();
            if overflowed || slot.staging.buffer == vk::Buffer::null() {
                let wanted = slot.wanted.max(slot.staging.size.wrapping_mul(2));
                let size = if wanted < least {
                    least
                } else if STAGING_MOST < wanted {
                    STAGING_MOST
                } else {
                    wanted
                };
                if size != slot.staging.size {
                    dev.free_staging(vk, &mut slot.staging);
                    slot.staging = dev.make_staging(vk, size).unwrap_or_default();
                }
            }
            let _ = vk.reset_command_pool(slot.pool, vk::CommandPoolResetFlags::empty());
        }
        slot.recording = false;
        slot.segment = 0;
        slot.used = 0;
        slot.wanted = 0;
        self.up.staging_hashed[n] = 0;
        self.up.open = Some(n);
    }

    /// MW2_MARK_EMPTY=1: the flat magenta texture, made the first time it is wanted.
    fn magenta_id(&mut self, vk: &Vk) -> u64 {
        if self.magenta != 0 {
            return self.magenta;
        }
        let mut flat = TextureData {
            header: TextureHeader {
                ok: true,
                width: 4,
                height: 4,
                blocks_wide: 4,
                blocks_high: 4,
                bytes_per_block: 4,
                expanded: true,
                ..TextureHeader::default()
            },
            bytes: vec![0; 4 * 4 * 4],
        };
        for texel in flat.bytes.chunks_exact_mut(4) {
            texel.copy_from_slice(&[0xFF, 0x00, 0xFF, 0xFF]); // R, G, B, A
        }
        // What a failure says; "?" when it said nothing.
        let mut error: &'static CStr = c"?";
        // 0x688 is the identity swizzle, XYZW; 0 would read (R,R,R,R) -- white.
        self.magenta = self.upload_one(vk, &flat, 6, 0, 0x688, Some(&mut error), 0, 0, false);
        if let Some(marker) = self.find_mut(self.magenta) {
            marker.pinned = true;
        }
        if self.magenta == NONE {
            log_warn!("textures: could not make the magenta marker: %s", error);
        }
        self.magenta
    }

    /// The texture a fetch constant describes, uploaded the first time it is seen and
    /// handed back, re-staged if its memory changed, after that. The id is the image and
    /// the sampler the constant asks for.
    pub fn upload(&mut self, vk: &Vk, fetch: &TextureFetch, error: Option<&mut &'static CStr>) -> u64 {
        if self.dev.device == vk::Device::null() && !self.initialise(vk) {
            if let Some(error) = error {
                *error = c"no device";
            }
            return NONE;
        }

        let key = FetchWords(fetch.d);
        if let Some(&found) = self.by_fetch.get(&key) {
            let (frame, segment) = (self.track.frame, self.track.segment);
            let (submission, began) = (self.track.submission, self.track.submission_began);
            if let Some(image) = self.find_mut(found) {
                image.last_bound = frame;
                image.bound_segment = segment;
                image.last_submission = submission;
                image.last_used_at = began;
            }
            self.refresh_if_changed(vk, found & IMAGE_BITS);
            return found;
        }
        self.upload_new(vk, fetch, key, error)
    }

    /// A lookup by_fetch does not hold, counted when it is one a frame's start forgot.
    #[cold]
    #[inline(never)]
    fn upload_new(&mut self, vk: &Vk, fetch: &TextureFetch, key: FetchWords, error: Option<&mut &'static CStr>) -> u64 {
        let retry = !self.retrying.is_empty() && self.retrying.remove(&key);
        let id = self.look_up(vk, fetch, key, error);
        if retry {
            if id == NONE {
                self.track.failures_failed_again = self.track.failures_failed_again.wrapping_add(1);
            } else {
                self.track.failures_made = self.track.failures_made.wrapping_add(1);
            }
        }
        id
    }

    fn look_up(&mut self, vk: &Vk, fetch: &TextureFetch, key: FetchWords, error: Option<&mut &'static CStr>) -> u64 {
        // The same texture under another sampler state: the image is held already.
        let content = FetchWords(content_of(fetch));
        if let Some(&held) = self.by_content.get(&content) {
            let held_image = &self.images[(held - 1) as usize];
            let sampler_state = point_only(sampler_key(fetch, held_image.min_level), held_image.filterable);
            let sampler = self.sampler_index_for(vk, sampler_state);
            if sampler == 0 {
                self.fails.fail(c"no sampler", error);
                self.by_fetch.insert(key, NONE);
                self.failed_keys.push(key);
                return NONE;
            }
            let image = &mut self.images[(held - 1) as usize];
            image.last_bound = self.track.frame;
            image.bound_segment = self.track.segment;
            image.last_submission = self.track.submission;
            image.last_used_at = self.track.submission_began;
            image.names.push(key.0);
            self.shared = self.shared.wrapping_add(1);
            self.refresh_if_changed(vk, held);
            let id = held | (u64::from(sampler) << SAMPLER_SHIFT);
            self.by_fetch.insert(key, id);
            return id;
        }

        let _timed = Timed::new(STUTTER_NEW_TEXTURES);
        // The base and the mip chain are two ranges with an address each.
        let extents = texture::extents_of(fetch);
        let base = host_address(extents.base_address);
        let mips = host_address(extents.mip_address);
        // What the fetch constant and the device alone refuse, refused before the memory is
        // watched or read: a failure made again at each frame's start (failed_keys) costs no
        // read then.
        if let Some(why) = self.refused_before_read(vk, fetch, base, mips) {
            self.fails.fail(why, error);
            self.by_fetch.insert(key, NONE);
            self.failed_keys.push(key);
            return NONE;
        }
        // Watched before it is read, so a write after the read shows.
        #[cfg(feature = "diagnostics")]
        note_source(
            extents.base_address,
            if base.is_null() { 0 } else { extents.base_bytes as usize },
            extents.mip_address,
            if mips.is_null() { 0 } else { extents.mip_bytes as usize },
        );
        let watch_stamp = track_source(
            self.track.frame,
            extents.base_address,
            if base.is_null() { 0 } else { extents.base_bytes as usize },
            extents.mip_address,
            if mips.is_null() { 0 } else { extents.mip_bytes as usize },
        );
        // SAFETY: guest memory at the fetch's own addresses.
        let data = unsafe { TextureData::read(fetch, base, mips) };
        if !data.header.ok {
            let why = if data.header.error.is_null() {
                c"unreadable"
            } else {
                // SAFETY: read_texture's errors are static C strings.
                unsafe { CStr::from_ptr::<'static>(data.header.error) }
            };
            self.fails.fail(why, error);
            self.by_fetch.insert(key, NONE);
            self.failed_keys.push(key);
            return NONE;
        }

        // Only the ranges actually read are watched.
        let base_bytes = if base.is_null() { 0 } else { extents.base_bytes as usize };
        let mip_bytes = if !mips.is_null() && data.levels().len() > 1 { extents.mip_bytes as usize } else { 0 };
        let signature = sign_source(extents.base_address, base_bytes, extents.mip_address, mip_bytes, None);
        let id = self.upload_one(
            vk,
            &data,
            fetch.format(),
            sampler_key(fetch, data.header.min_level),
            fetch.swizzle(),
            error,
            signature,
            base_bytes,
            fetch.linearises(),
        );
        let mut named = id;
        if self.find(id).is_some() {
            let image = &mut self.images[(id - 1) as usize];
            image.min_level = data.header.min_level;
            image.content = content.0;
            image.names.push(key.0);
            self.by_content.insert(content, id);
            let filterable = self.images[(id - 1) as usize].filterable;
            let index = self.sampler_index_for(vk, point_only(sampler_key(fetch, data.header.min_level), filterable));
            named |= u64::from(index) << SAMPLER_SHIFT;
            let image = &mut self.images[(id - 1) as usize];
            image.address = if base_bytes != 0 { extents.base_address } else { 0 };
            image.mip_address = if mip_bytes != 0 { extents.mip_address } else { 0 };
            image.mip_bytes = mip_bytes;
            image.xenos_format = fetch.format();
            image.watch_stamp = watch_stamp;
            image.fetch_words = fetch.d;
            #[cfg(feature = "diagnostics")]
            if count_past_rows() {
                let (rows, mip_rows) = texture::row_extents(fetch, &extents);
                let image = &mut self.images[(id - 1) as usize];
                let (narrow, narrow_mips) = ((rows as usize).min(base_bytes), (mip_rows as usize).min(mip_bytes));
                if narrow < base_bytes || narrow_mips < mip_bytes {
                    image.narrow_bytes = narrow;
                    image.narrow_mip_bytes = narrow_mips;
                    image.narrow_signature = sign_narrow(image);
                }
            }
            self.note_emptiness(id, &data);
        }
        // A failure is cached too, until the next frame's start (failed_keys): a texture bound
        // every draw would otherwise fail thousands of times a frame.
        self.by_fetch.insert(key, named);
        if named == NONE {
            self.failed_keys.push(key);
        }
        named
    }

    /// The refusal upload_one or make_image would make of this fetch constant's texture after
    /// it is read, where only the fetch constant and the device decide it, in their order: no
    /// Vulkan equivalent, larger than the device allows, a format the device cannot sample, a
    /// cube map that is not square. Xenia's order: FindOrCreateTexture refuses the size and
    /// CreateTexture the host format before LoadTexturesData reads anything (texture_cache.cc,
    /// vulkan_texture_cache.cc). None where read_texture itself refuses first (it reads
    /// nothing then either) and without a device, as upload_one says that. A texture refused
    /// here past the host format's choice is counted in linearised_refused as upload_one
    /// counts it, which it no longer reaches.
    fn refused_before_read(
        &mut self,
        vk: &Vk,
        fetch: &TextureFetch,
        base: *const u8,
        mips: *const u8,
    ) -> Option<&'static CStr> {
        if self.dev.device == vk::Device::null() {
            return None;
        }
        let header = texture::texture_header(fetch, base, mips);
        if !header.error.is_null() {
            return None;
        }
        let direct = formats::vulkan_format_for(fetch.format());
        if direct == vk::Format::UNDEFINED.as_raw() {
            return Some(c"no Vulkan equivalent for the format");
        }
        let physical = self.dev.physical;
        // SAFETY: the cache's device's physical device.
        let features = |format: vk::Format| unsafe { vk.format_features(physical, format) };
        // Chosen where upload_one chooses it, before the size, which it does not change.
        let (format, _, srgb_refused) = host_format(direct, fetch.linearises(), features);
        // make_image's own question, asked the same way.
        let sampled = || {
            // SAFETY: the cache's device's physical device.
            let properties =
                unsafe { vk.get_physical_device_format_properties(physical, vk::Format::from_raw(format)) };
            properties.optimal_tiling_features.contains(vk::FormatFeatureFlags::SAMPLED_IMAGE)
        };
        let why = if self.dev.too_large(&header) {
            Some(c"larger than the device allows")
        } else if !sampled() {
            Some(c"device cannot sample this format")
        } else if !header.volume && header.layers == 6 && header.width != header.height {
            Some(c"a cube map that is not square")
        } else {
            None
        };
        if why.is_some() && srgb_refused {
            self.linearised_refused = self.linearised_refused.wrapping_add(1);
        }
        why
    }

    /// Diagnostics: a changed texture's bytes re-read or let go; counted when the change
    /// lay wholly past what row_extents named, which the signature over those alone would
    /// not have seen.
    #[cfg(feature = "diagnostics")]
    fn note_changed(&mut self, index: usize) {
        let image = &mut self.images[index];
        if image.narrow_bytes == 0 && image.narrow_mip_bytes == 0 {
            return;
        }
        let now = sign_narrow(image);
        if now == image.narrow_signature {
            self.track.changed_past_rows = self.track.changed_past_rows.wrapping_add(1);
        }
        image.narrow_signature = now;
    }

    /// Whether what was just read into this image looks like a block nothing has written,
    /// measured on the decoded texture. An image that becomes empty is queued to be read
    /// again at the end of its segment.
    fn note_emptiness(&mut self, id: u64, data: &TextureData) {
        if data.bytes.is_empty() {
            return;
        }
        self.note_emptiness_of(id, empty_percent(&data.bytes));
    }

    /// note_emptiness of a texture `empty` percent of whose bytes, as read_texture lays
    /// them out, are in empty 64-byte spans (empty_percent).
    fn note_emptiness_of(&mut self, id: u64, empty: u8) {
        let frame = self.track.frame;
        let image = &mut self.images[(id - 1) as usize];
        if image.address == 0 && image.mip_address == 0 {
            return;
        }
        if empty <= 25 {
            image.from_an_empty_block = false;
            return;
        }
        if empty <= 60 {
            return;
        }
        if !image.from_an_empty_block && self.track.empty_this_segment.len() < EMPTY_PER_SEGMENT {
            self.track.empty_this_segment.push(id);
        }
        image.from_an_empty_block = true;
        if image.empty_since == 0 {
            image.empty_since = if frame != 0 { frame } else { 1 };
        }
    }

    /// Staged over the old contents if it still has their shape. False when it could not
    /// be.
    fn restage(&mut self, vk: &Vk, id: u64, data: &TextureData) -> bool {
        let Self { up, dev, fails, images, .. } = self;
        let image = &mut images[(id - 1) as usize];
        if !data.header.ok {
            return false;
        }
        // In the host format's layout, as upload_one staged it.
        let converted;
        let bytes = if image.convert != Convert::None {
            let mut levels = Vec::new();
            converted = texture_convert::convert(image.convert, data.levels(), &data.bytes, &mut levels);
            &converted
        } else {
            &data.bytes
        };
        // SAFETY: the cache's device, the image its; the bytes live through the call.
        let staged = bytes.len() as u64 == image.bytes
            && unsafe { up.stage_into(vk, dev, fails, image, bytes.as_ptr(), bytes.len(), None) };
        if staged {
            // The parts' signatures are of what was uploaded before; the caller signs anew.
            image.parts = None;
            image.signature_stale = false;
        }
        staged
    }

    /// The fetch constant is unchanged when only the bytes are, so the cache would
    /// otherwise hand back the upload taken the first time the texture was bound.
    fn refresh_if_changed(&mut self, vk: &Vk, id: u64) {
        let (frame, segment) = (self.track.frame, self.track.segment);
        let Some(image) = self.find(id) else { return };
        if image.image == vk::Image::null() || image.borrowed || (image.source_bytes == 0 && image.mip_bytes == 0) {
            return;
        }
        let index = ((id & IMAGE_BITS) - 1) as usize;
        // A rewritten one is read again at the end of every segment that binds it,
        // whatever happens here: the title can write it after this bind, and a segment
        // only ever sees what was read at its own end.
        let queue_for_segment = |track: &mut Tracking, image: &mut Image| {
            if image.queued_segment == segment || track.rewritten_this_segment.len() >= REWRITTEN_PER_SEGMENT {
                return;
            }
            image.queued_segment = segment;
            track.rewritten_this_segment.push(id);
        };
        if self.images[index].rewritten {
            queue_for_segment(&mut self.track, &mut self.images[index]);
        }
        let image = &self.images[index];
        if image.checked_frame == frame {
            return;
        }
        if unchanged(image) {
            self.images[index].checked_frame = frame;
            self.track.unchanged_by_watch = self.track.unchanged_by_watch.wrapping_add(1);
            return;
        }
        // Watched, and written since: due now, whatever the schedule says.
        if image.watch_stamp == 0 && frame.wrapping_sub(image.checked_frame) < image.sign_period {
            return;
        }
        self.refresh(vk, index, queue_for_segment);
    }

    #[cold]
    #[inline(never)]
    fn refresh(&mut self, vk: &Vk, index: usize, queue_for_segment: impl Fn(&mut Tracking, &mut Image)) {
        let frame = self.track.frame;
        self.images[index].checked_frame = frame;
        self.track.resigned = self.track.resigned.wrapping_add(1);
        let _timed = Timed::new(STUTTER_REFRESHED_TEXTURES);

        #[cfg(feature = "diagnostics")]
        note_image(&self.images[index]);
        match self.refresh_in_parts(vk, index) {
            Some(false) => return,
            Some(true) => {
                let image = &mut self.images[index];
                image.rewritten = true;
                image.sign_period = 1;
                queue_for_segment(&mut self.track, &mut self.images[index]);
                #[cfg(feature = "diagnostics")]
                self.note_changed(index);
                self.track.refreshed = self.track.refreshed.wrapping_add(1);
                return;
            }
            None => {}
        }
        self.images[index].watch_stamp = track_image(frame, &self.images[index]);
        let now = sign_image(&self.images[index], Some(&mut self.track.sign_nanos));
        let image = &mut self.images[index];
        if now == image.signature && !image.signature_stale {
            // An image that came up empty is checked every frame until it holds a
            // picture: the point is to catch the moment its bytes move.
            if !image.rewritten && !image.from_an_empty_block && image.sign_period < SIGN_PERIOD_MAX {
                image.sign_period *= 2;
            }
            return;
        }
        image.rewritten = true;
        image.sign_period = 1;
        queue_for_segment(&mut self.track, &mut self.images[index]);

        let data = read_again(&self.images[index]);
        if !self.restage(vk, index as u64 + 1, &data) {
            return;
        }
        #[cfg(feature = "diagnostics")]
        self.note_changed(index);
        self.images[index].signature = now;
        self.track.refreshed = self.track.refreshed.wrapping_add(1);
        self.note_emptiness(index as u64 + 1, &data);
    }

    /// A watched texture that has changed before, read again in parts (texture_parts.rs):
    /// the parts with a page written since the last read are read, and those whose bytes
    /// changed uploaded into the image as it is. None when it cannot be read so, and the
    /// whole read follows; otherwise whether anything changed. Cut into parts the first
    /// time, which reads and uploads every part.
    fn refresh_in_parts(&mut self, vk: &Vk, index: usize) -> Option<bool> {
        let image = &self.images[index];
        // A conversion or DXT3A's expansion changes the bytes' layout.
        if !image.rewritten
            || image.watch_stamp == 0
            || image.convert != Convert::None
            || image.xenos_format == 58
            || image.borrowed
            || image.levels.is_empty()
            || self.up.open.is_none()
        {
            return None;
        }
        let since = image.watch_stamp;
        // Armed again before anything is read: a write from now on shows at the next
        // re-read, and one made since `since` shows as a page armed after it.
        let stamp = track_image(self.track.frame, image);
        self.images[index].watch_stamp = stamp;
        if stamp == 0 {
            return None;
        }
        let image = &self.images[index];
        let fetch = TextureFetch { d: image.fetch_words };
        let bytes_per_block = texture::texture_format_of(image.xenos_format).bits_per_block / 8;
        let base = if image.source_bytes != 0 { image.address } else { 0 };
        let mips = if image.mip_bytes != 0 { image.mip_address } else { 0 };
        let (base_at, mips_at) = (host_address(base), host_address(mips));
        let first = image.parts.is_none();
        if first {
            // Not cut until it has changed: until then, the whole read's test of a change,
            // its signature, after the arming as there.
            let now = sign_image(image, Some(&mut self.track.sign_nanos));
            if now == image.signature && !image.signature_stale {
                return Some(false);
            }
            self.images[index].signature = now;
        }
        let mut parts = match self.images[index].parts.take() {
            Some(parts) => parts,
            None => {
                self.parts.cut = self.parts.cut.wrapping_add(1);
                let image = &self.images[index];
                let mut parts =
                    Box::new(texture_parts::cut(&fetch, &image.levels, base, mips, bytes_per_block, image.volume));
                parts.mirror = vec![0; image.bytes as usize];
                parts.empty_spans = parts.mirror.len() / 64;
                parts
            }
        };
        let work = &mut self.parts;
        work.rereads = work.rereads.wrapping_add(1);
        parts.rereads = parts.rereads.wrapping_add(1);
        work.written.clear();
        work.written.extend(
            parts.pages.iter().map(|&page| first || !bridge::watch::unwritten(page * texture_parts::PAGE, texture_parts::PAGE, since)),
        );
        work.bytes.clear();
        work.regions.clear();
        let read_before = parts.read;
        let image = &self.images[index];
        // Most of it written (a video's frame, a texture streamed in again): one whole
        // read and one copy cost less than a read and a region a part.
        let touched: Vec<bool> = if first {
            Vec::new()
        } else {
            (0..parts.list.len()).map(|i| parts.touched(i, &work.written)).collect()
        };
        if !first && touched.iter().filter(|&&t| t).count() * 2 > parts.list.len() {
            return self.reread_whole_in_parts(vk, index, parts);
        }
        for i in 0..parts.list.len() {
            if !first && !touched[i] {
                continue;
            }
            let part = &parts.list[i];
            work.scratch.clear();
            work.scratch.resize(part.bytes, 0);
            // SAFETY: guest memory at the texture's own addresses, as its whole read takes
            // them; the part is one of its levels'.
            if !unsafe { texture::read_region(&fetch, base_at, mips_at, part.level, part.rect, &mut work.scratch) } {
                // Read whole instead, and cut again next time.
                return None;
            }
            work.read = work.read.wrapping_add(1);
            parts.read = parts.read.wrapping_add(1);
            // SAFETY: the scratch, `part.bytes` long.
            let signature = unsafe { signature_of(work.scratch.as_ptr(), work.scratch.len()) };
            if !first && signature == part.signature {
                continue;
            }
            let level = &image.levels[part.level as usize];
            let at = (work.bytes.len() + 15) & !15;
            work.bytes.resize(at, 0);
            work.regions.push(texture_parts::region(
                part,
                level,
                image.block_width,
                image.block_height,
                image.volume,
                at as u64,
            ));
            work.bytes.extend_from_slice(&work.scratch);
            parts.list[i].signature = signature;
            texture_parts::place_counted(&mut parts, i, level, bytes_per_block, &work.scratch, &mut work.spans);
        }
        if cfg!(feature = "diagnostics") {
            let counts = work
                .by_texture
                .entry((image.address, image.width, image.height, image.xenos_format))
                .or_insert([0; 5]);
            counts[0] = parts.list.len() as u64;
            counts[4] = parts.mirror.len() as u64;
            counts[1] += 1;
            counts[2] += parts.read - read_before;
            counts[3] += work.regions.len() as u64;
        }
        if work.regions.is_empty() {
            self.images[index].parts = Some(parts);
            return Some(false);
        }
        if check_parts() {
            if parts.copied.len() != parts.mirror.len() {
                parts.copied = vec![0; parts.mirror.len()];
            }
            let image = &self.images[index];
            texture_parts::apply_copy(
                &work.regions,
                &work.bytes,
                &image.levels,
                image.block_width,
                image.block_height,
                bytes_per_block,
                image.volume,
                &mut parts.copied,
            );
        }
        let Self { up, dev, fails, images, parts: work, .. } = self;
        let image = &mut images[index];
        // SAFETY: the cache's device, the image its.
        if !unsafe { up.stage_parts(vk, dev, fails, image, &work.bytes, &mut work.regions) } {
            // What the signatures say was uploaded was not: read whole.
            return None;
        }
        work.uploaded = work.uploaded.wrapping_add(work.regions.len() as u64);
        parts.uploaded = parts.uploaded.wrapping_add(work.regions.len() as u64);
        work.uploaded_bytes = work.uploaded_bytes.wrapping_add(work.bytes.len() as u64);
        image.signature_stale = true;
        // Whether it is still mostly empty, from the bytes as now uploaded.
        let empty = texture_parts::empty_percent(&parts);
        image.parts = Some(parts);
        self.note_emptiness_of(index as u64 + 1, empty);
        if check_parts() {
            self.check_parts(index, stamp);
        }
        Some(true)
    }

    /// A texture cut into parts read again whole, most of it having been written: uploaded
    /// whole if it changed, the parts kept, their copy of the bytes refreshed and their
    /// signatures cleared, so that the next re-read in parts uploads every part it reads.
    fn reread_whole_in_parts(&mut self, vk: &Vk, index: usize, mut parts: Box<texture_parts::Parts>) -> Option<bool> {
        let data = read_again(&self.images[index]);
        let work = &mut self.parts;
        work.whole = work.whole.wrapping_add(1);
        if !data.header.ok || data.bytes.len() != parts.mirror.len() {
            return None;
        }
        if data.bytes == parts.mirror {
            self.images[index].parts = Some(parts);
            return Some(false);
        }
        let Self { up, dev, fails, images, .. } = self;
        let image = &mut images[index];
        // SAFETY: the cache's device, the image its; the bytes live through the call.
        if data.bytes.len() as u64 != image.bytes
            || !unsafe { up.stage_into(vk, dev, fails, image, data.bytes.as_ptr(), data.bytes.len(), None) }
        {
            return None;
        }
        parts.mirror.copy_from_slice(&data.bytes);
        parts.empty_spans = texture_parts::empty_spans(&parts.mirror);
        if check_parts() {
            parts.copied = data.bytes.clone();
        }
        for part in &mut parts.list {
            part.signature = 0;
        }
        image.signature_stale = true;
        image.parts = Some(parts);
        self.note_emptiness(index as u64 + 1, &data);
        Some(true)
    }

    /// MW2_CHECK_TEXTURE_PARTS: the texture read whole, against what its re-reads in parts
    /// uploaded. A part that differs with none of its pages written since `stamp` was
    /// missed: something wrote it that the watch did not see.
    #[cold]
    fn check_parts(&mut self, index: usize, stamp: u64) {
        let data = read_again(&self.images[index]);
        let image = &mut self.images[index];
        let Some(parts) = image.parts.as_mut() else { return };
        let work = &mut self.parts;
        work.checked = work.checked.wrapping_add(1);
        let counted = texture_parts::empty_spans(&parts.mirror);
        if counted != parts.empty_spans {
            work.miscounted = work.miscounted.wrapping_add(1);
            parts.empty_spans = counted;
        }
        if parts.copied != parts.mirror {
            let differ = parts.copied.iter().zip(&parts.mirror).filter(|(a, b)| a != b).count();
            work.misplaced_bytes = work.misplaced_bytes.wrapping_add(differ as u64);
            if work.misplaced_bytes == differ as u64 {
                log_warn!(
                    "textures: the copy of the parts of %ux%u fmt %u at %08X puts %u bytes elsewhere than the parts' bytes say",
                    image.width,
                    image.height,
                    image.xenos_format,
                    image.address,
                    differ as u32
                );
            }
            parts.copied.clone_from(&parts.mirror);
        }
        if !data.header.ok || data.bytes.len() != parts.mirror.len() || data.bytes == parts.mirror {
            return;
        }
        let bytes_per_block = texture::texture_format_of(image.xenos_format).bits_per_block / 8;
        let written: Vec<bool> =
            parts.pages.iter().map(|&page| !bridge::watch::unwritten(page * texture_parts::PAGE, texture_parts::PAGE, stamp)).collect();
        let (mut read, mut uploaded) = (Vec::new(), Vec::new());
        for i in 0..parts.list.len() {
            let part = &parts.list[i];
            let level = &image.levels[part.level as usize];
            read.resize(part.bytes, 0);
            uploaded.resize(part.bytes, 0);
            texture_parts::take(part, level, bytes_per_block, &data.bytes, &mut read);
            texture_parts::take(part, level, bytes_per_block, &parts.mirror, &mut uploaded);
            if read == uploaded {
                continue;
            }
            if parts.touched(i, &written) {
                work.raced_parts = work.raced_parts.wrapping_add(1);
                continue;
            }
            let differ = read.iter().zip(&uploaded).filter(|(a, b)| a != b).count();
            work.missed_parts = work.missed_parts.wrapping_add(1);
            work.missed_bytes = work.missed_bytes.wrapping_add(differ as u64);
            if work.missed_parts <= 16 {
                log_warn!(
                    "textures: a part of %ux%u fmt %u at %08X read in parts is stale: level %u, blocks %u..%u x %u..%u x %u..%u, %u bytes differ, none of its pages written since",
                    image.width,
                    image.height,
                    image.xenos_format,
                    image.address,
                    part.level,
                    part.rect.x0,
                    part.rect.x1,
                    part.rect.y0,
                    part.rect.y1,
                    part.rect.z0,
                    part.rect.z1,
                    differ as u32
                );
            }
            // Counted once: the next comparison starts from what memory holds.
            texture_parts::place(part, level, bytes_per_block, &read, &mut parts.mirror);
        }
    }

    fn refresh_rewritten(&mut self, vk: &Vk) {
        let again = std::mem::take(&mut self.track.rewritten_this_segment);
        for id in again {
            let Some(image) = self.find(id) else { continue };
            if image.image == vk::Image::null() || image.borrowed || (image.source_bytes == 0 && image.mip_bytes == 0) {
                continue;
            }
            if unchanged(image) {
                self.track.unchanged_by_watch = self.track.unchanged_by_watch.wrapping_add(1);
                continue;
            }
            let index = ((id & IMAGE_BITS) - 1) as usize;
            self.track.reread_rewritten = self.track.reread_rewritten.wrapping_add(1);
            #[cfg(feature = "diagnostics")]
            note_image(&self.images[index]);
            match self.refresh_in_parts(vk, index) {
                Some(false) => continue,
                Some(true) => {
                    #[cfg(feature = "diagnostics")]
                    self.note_changed(index);
                    if cfg!(feature = "diagnostics") {
                        self.track.late_restages.push(id);
                    }
                    self.track.late_reads.push(LateRead { id, frame: self.track.frame });
                    self.track.changed_after_first_bind = self.track.changed_after_first_bind.wrapping_add(1);
                    continue;
                }
                None => {}
            }
            self.images[index].watch_stamp = track_image(self.track.frame, &self.images[index]);
            let now = sign_image(&self.images[index], Some(&mut self.track.sign_nanos));
            if now == self.images[index].signature && !self.images[index].signature_stale {
                continue;
            }
            let data = read_again(&self.images[index]);
            if !self.restage(vk, index as u64 + 1, &data) {
                continue;
            }
            #[cfg(feature = "diagnostics")]
            self.note_changed(index);
            self.images[index].signature = now;
            if cfg!(feature = "diagnostics") {
                self.track.late_restages.push(id);
            }
            self.track.late_reads.push(LateRead { id, frame: self.track.frame });
            self.track.changed_after_first_bind = self.track.changed_after_first_bind.wrapping_add(1);
        }
    }

    fn refresh_empty(&mut self, vk: &Vk) {
        let again = std::mem::take(&mut self.track.empty_this_segment);
        for id in again {
            let Some(image) = self.find(id) else { continue };
            if image.image == vk::Image::null() || image.borrowed || !image.from_an_empty_block {
                continue;
            }
            let index = ((id & IMAGE_BITS) - 1) as usize;
            #[cfg(feature = "diagnostics")]
            note_image(&self.images[index]);
            self.images[index].watch_stamp = track_image(self.track.frame, &self.images[index]);
            let data = read_again(&self.images[index]);
            if data.header.ok && empty_percent(&data.bytes) <= 25 {
                if !self.restage(vk, index as u64 + 1, &data) {
                    continue;
                }
                if cfg!(feature = "diagnostics") {
                    self.track.late_restages.push(id);
                }
                let signature = sign_image(&self.images[index], None);
                let image = &mut self.images[index];
                image.signature = signature;
                image.from_an_empty_block = false;
                #[cfg(feature = "diagnostics")]
                if image.narrow_bytes != 0 || image.narrow_mip_bytes != 0 {
                    image.narrow_signature = sign_narrow(image);
                }
                self.track.refilled_before_submit = self.track.refilled_before_submit.wrapping_add(1);
                continue;
            }
            // Reaches the screen with nothing in it. tools/flash_hunt.sh matches these
            // lines with the flashes somebody saw.
            self.track.still_empty_at_submit = self.track.still_empty_at_submit.wrapping_add(1);
            if self.track.still_empty_at_submit <= 24 || flash_hunt() {
                let image = &self.images[index];
                let mut moves = [0u8; 1024];
                outside::describe_moves_around(image.address, image.source_bytes as u32, 16, &mut moves);
                let moves = CStr::from_bytes_until_nul(&moves).unwrap_or(c"");
                log_info!(
                    "textures:   still empty at submit, %ux%u fmt %u at %08X (%uKB), frame %llu: %s",
                    image.width,
                    image.height,
                    image.xenos_format,
                    image.address,
                    (image.source_bytes / 1024) as u32,
                    self.track.frame,
                    moves
                );
            }
        }
    }

    /// How many times the image behind `id` has been staged (0 for a borrowed one).
    pub fn version(&self, id: u64) -> u32 {
        match self.find(id) {
            Some(image) if !image.borrowed => image.staged,
            _ => 0,
        }
    }

    /// The ids re-staged at a segment's end since the last call.
    pub fn take_late_restages(&mut self, ids: &mut Vec<u64>) {
        std::mem::swap(ids, &mut self.track.late_restages);
        self.track.late_restages.clear();
    }

    /// Whether an image read from [physical, physical + size) has been bound in the
    /// segment being recorded.
    pub fn bound_in_this_segment(&self, physical: u32, size: u32) -> bool {
        // A linear walk, as memory_written's.
        let end = physical.wrapping_add(size);
        let overlaps = |address: u32, bytes: usize| {
            bytes != 0 && address < end && u64::from(address).wrapping_add(bytes as u64) > u64::from(physical)
        };
        self.images.iter().any(|image| {
            image.image != vk::Image::null()
                && !image.borrowed
                && image.bound_segment == self.track.segment
                && (overlaps(image.address, image.source_bytes) || overlaps(image.mip_address, image.mip_bytes))
        })
    }

    /// Ends a segment: its rewritten and empty textures read again, and the command
    /// buffer to submit ahead of its draws, or null when nothing was uploaded.
    pub fn end_segment(&mut self, vk: &Vk) -> vk::CommandBuffer {
        let last = self.track.ending_submission;
        {
            let _timed = Timed::new(STUTTER_REFRESHED_TEXTURES);
            self.refresh_rewritten(vk);
            self.refresh_empty(vk);
        }
        self.track.segment = self.track.segment.wrapping_add(1);
        self.track.epoch = self.track.epoch.wrapping_add(1);
        self.track.segments_ended = self.track.segments_ended.wrapping_add(1);
        let open = self.up.open;
        if last {
            self.up.open = None;
        }
        // A segment that uploaded nothing leaves its buffer to the next.
        let Some(index) = open else { return vk::CommandBuffer::null() };
        if !self.up.slots[index].recording {
            return vk::CommandBuffer::null();
        }
        self.up.slots[index].recording = false;
        #[cfg(feature = "diagnostics")]
        {
            // The staging this segment's uploads read, which is S1's untiling on the
            // game's data.
            let slot = &self.up.slots[index];
            let from = self.up.staging_hashed[index];
            let to = from.max(slot.used);
            self.up.staging_hashed[index] = to;
            if super::trace::wanted() {
                // Folded in order, FNV-style, so that two equal buffers do not cancel out.
                let mut overflow = super::trace::fnv64(&[]);
                for extra in &slot.overflow {
                    // SAFETY: the extra buffer's own mapping, `size` long.
                    let hash = unsafe { super::trace::fnv_write_combined(extra.mapped, extra.size as usize) };
                    overflow = (overflow ^ hash.wrapping_add(extra.size)).wrapping_mul(0x100_0000_01b3);
                }
                let staged = if slot.staging.mapped.is_null() {
                    0
                } else {
                    // SAFETY: the slot's mapping holds `used` bytes from its start.
                    unsafe {
                        super::trace::fnv_write_combined(slot.staging.mapped.add(from as usize), (to - from) as usize)
                    }
                };
                super::trace::note(&format!(
                    "UploadSlot slot={} segment={} from={} to={} fnv={:016x} overflow={} overflowfnv={:016x}",
                    index,
                    slot.segment,
                    from,
                    to,
                    staged,
                    slot.overflow.len(),
                    overflow
                ));
            }
        }
        let slot = &mut self.up.slots[index];
        let uploads = slot.commands[slot.segment];
        slot.segment += 1;
        self.track.segments_with_uploads = self.track.segments_with_uploads.wrapping_add(1);
        // SAFETY: the cache's device; the buffer is recording (begun by record_upload).
        if unsafe { vk.end_command_buffer(uploads) } != vk::Result::SUCCESS {
            return vk::CommandBuffer::null();
        }
        uploads
    }

    /// The last segment of a submission, which closes the open slot.
    pub fn end_submission(&mut self, vk: &Vk) -> vk::CommandBuffer {
        self.track.ending_submission = true;
        let uploads = self.end_segment(vk);
        self.track.ending_submission = false;
        uploads
    }

    /// A draw into the world pass bound this texture. Returns the id to bind: normally the
    /// one passed in; under MW2_MARK_EMPTY=1 a flat magenta texture when this one was read
    /// from a block with nothing in it.
    pub fn note_drawn_in_world(&mut self, vk: &Vk, id: u64) -> u64 {
        let frame = self.track.frame;
        let young = {
            let Some(image) = self.find_mut(id) else { return id };
            if !image.from_an_empty_block {
                return id;
            }
            let counted = !image.counted_drawn;
            image.counted_drawn = true;
            // Only while the texture is young: some world textures are legitimately empty
            // and bound every frame for ever.
            let young = image.empty_since != 0 && frame.wrapping_sub(image.empty_since) < 60;
            if counted {
                self.track.empty_drawn_in_world = self.track.empty_drawn_in_world.wrapping_add(1);
            }
            young
        };
        if !mark_empty() || !young {
            return id;
        }
        let magenta = self.magenta_id(vk);
        if magenta == NONE {
            return id;
        }
        self.marked_empty = self.marked_empty.wrapping_add(1);
        magenta
    }

    /// A render target the title resolved out to memory and samples back. The image is
    /// the caller's, so the entry only borrows its view; `key` names the copy, so an id
    /// survives across frames, and so does its entry in the set.
    pub fn adopt(&mut self, vk: &Vk, key: u64, view: vk::ImageView, fetch: &TextureFetch, filterable: bool) -> u64 {
        if self.dev.device == vk::Device::null() && !self.initialise(vk) {
            return NONE;
        }
        // A resolve's copy is one level, whatever the fetch constant says; sampled with
        // point filters when its format cannot be filtered (format_support.rs).
        let sampler = self.sampler_index_for(vk, point_only(sampler_key(fetch, 0), filterable));
        if sampler == 0 || view == vk::ImageView::null() {
            return NONE;
        }
        let found = self.adopted.get(&key).copied();
        let id = match found {
            Some(id) => id,
            None => self.new_slot(),
        };
        if id == 0 {
            return NONE;
        }
        if found.is_none() {
            self.adopted.insert(key, id);
        }
        let Self { images, bind, .. } = self;
        // shutdown keeps `adopted` (and `magenta`), so after a second initialise an old key
        // names a stale id. Past the end of `images` this index aborts. Within it (new
        // uploads refilled `images`) the entry aliases a live texture: it turns borrowed and takes
        // this view, so its own image, view and memory leak and its set entry samples the
        // copy (a stale `magenta` likewise names an unrelated texture). The game initialises
        // once (shutdown runs only at exit).
        let image = &mut images[(id - 1) as usize];
        image.borrowed = true;
        image.view = view;
        // A copy's view changes only once the GPU is done with the old one (forget_views),
        // so nothing pending reads the entry being rewritten.
        if image.written != image.view {
            // SAFETY: the cache's device; see above.
            unsafe { bind.write_image(vk, element_of(id), image) };
        }
        id | (u64::from(sampler) << SAMPLER_SHIFT)
    }

    /// The caller is destroying views it lent to adopt, with the GPU idle. Each adopted
    /// entry is written again at its next adopt, because a replacement view can come back
    /// with the very same handle.
    pub fn forget_views(&mut self) {
        for image in self.images.iter_mut() {
            if image.borrowed {
                image.written = vk::ImageView::null();
            }
        }
    }

    /// What a draw's record holds for a slot bound to `id`: the image's place in set 1 and
    /// its sampler's. `kind` is what the shaders declare the slot as; nothing, or a texture
    /// of another kind, is the declared kind's 1x1 stand-in.
    #[inline]
    pub fn handle(&mut self, id: u64, kind: u8) -> u32 {
        // None when nothing is bound or it was released, false for another kind.
        let usable =
            self.find(id).and_then(|image| (image.written != vk::ImageView::null()).then(|| kind_of(image) == kind));
        if usable == Some(false) {
            self.kind_mismatches = self.kind_mismatches.wrapping_add(1);
        }
        match self.find(id) {
            Some(image) if usable == Some(true) => {
                element_of(id) | (self.sampler_slot_of(id, image) << bindings::HANDLE_IMAGE_BITS)
            }
            _ => u32::from(kind) | (self.sampler_slot_of(id, self.fallback_of(kind)) << bindings::HANDLE_IMAGE_BITS),
        }
    }

    /// Set 1: bound once a command buffer.
    pub fn set(&self) -> vk::DescriptorSet {
        self.bind.set
    }

    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code, reason = "the report and the diagnostic keys' alone"))]
    pub fn report(&self) {
        if !cfg!(feature = "diagnostics") {
            return;
        }
        if self.uploads == 0 && self.fails.failures == 0 {
            return;
        }
        log_info!(
            "textures: %u uploaded (%llu KB), %u failed, %llu descriptors written, %llu bound as a fallback because the shader declares another kind",
            self.uploads,
            self.uploaded_bytes / 1024,
            self.fails.failures,
            self.bind.written,
            self.kind_mismatches
        );
        for (&reason, &count) in &self.fails.reasons {
            log_info!("textures:   %6u  %s", count, reason);
        }
        if self.converted != 0 || self.point_only != 0 {
            log_info!(
                "textures: %llu converted for a format the device cannot sample, %llu sampled with point filters for one it cannot filter",
                self.converted,
                self.point_only
            );
        }
        if self.linearised_refused != 0 {
            log_info!("textures: %llu asked to be linearised in a format with no sRGB form", self.linearised_refused);
        }
        if self.up.batched != 0 || self.up.alone != 0 {
            log_info!(
                "textures: %llu uploads went ahead of the submission that needed them, %llu were submitted on their own; %llu overflowed their slot's staging",
                self.up.batched,
                self.up.alone,
                self.up.overflows
            );
        }
        let t = &self.track;
        if t.resigned != 0 {
            log_info!(
                "textures: %llu signings (%.1f ms in all), %llu re-staged because the guest had rewritten them, %llu signed early because the image pool copied onto them",
                t.resigned,
                t.sign_nanos as f64 / 1e6,
                t.refreshed,
                t.invalidated_by_writes
            );
        }
        if t.unchanged_by_watch != 0 {
            log_info!("textures: %llu checks answered by the write watch, nothing signed", t.unchanged_by_watch);
        }
        let w = &self.parts;
        if w.rereads != 0 {
            log_info!(
                "textures: %llu re-reads in parts of %llu textures cut into parts (%llu of them whole, most of it written): %llu parts read, %llu uploaded (%llu KB)",
                w.rereads,
                w.cut,
                w.whole,
                w.read,
                w.uploaded,
                w.uploaded_bytes / 1024
            );
        }
        if !w.by_texture.is_empty() {
            let once = w.by_texture.values().filter(|counts| counts[1] <= 1).count();
            let copies: u64 = w.by_texture.values().map(|counts| counts[4]).sum();
            log_info!(
                "textures: %u textures cut into parts, %u of them never read in parts again; their copies %llu KB in all",
                w.by_texture.len() as u32,
                once as u32,
                copies / 1024
            );
        }
        // The textures read in parts the most.
        let mut most: Vec<_> = w.by_texture.iter().collect();
        most.sort_by_key(|(_, counts)| std::cmp::Reverse(counts[2]));
        for (&(address, width, height, format), counts) in most.into_iter().take(8) {
            log_info!(
                "textures:   %ux%u fmt %u at %08X in %llu parts: %llu re-reads in parts read %llu parts and uploaded %llu",
                width,
                height,
                format,
                address,
                counts[0],
                counts[1],
                counts[2],
                counts[3]
            );
        }
        if w.checked != 0 {
            log_info!(
                "textures: %llu re-reads in parts checked against a whole read: %llu parts stale with none of their pages written since (%llu bytes), %llu written after their read; %llu bytes misplaced by the copies; %llu miscounts of the empty spans",
                w.checked,
                w.missed_parts,
                w.missed_bytes,
                w.raced_parts,
                w.misplaced_bytes,
                w.miscounted
            );
        }
        if t.reread_rewritten != 0 {
            log_info!(
                "textures: rewritten textures signed again at a segment's end %llu times; %llu had changed since the frame's first bind and were re-staged",
                t.reread_rewritten,
                t.changed_after_first_bind
            );
        }
        log_info!(
            "textures: of those, %llu bound again the frame after, %llu on their last use (the title had freed them: a frame drawn with whatever replaced them)",
            t.late_reads_bound_again,
            t.late_reads_on_last_use
        );
        log_info!(
            "textures: %llu segments ended, %llu of them with uploads of their own",
            t.segments_ended,
            t.segments_with_uploads
        );
        if t.empty_drawn_in_world != 0 || t.refilled_before_submit != 0 || t.still_empty_at_submit != 0 {
            log_info!(
                "textures: %llu world textures drawn from a block that looked empty; read again at their segment's end, %llu were put right and %llu reached the screen empty",
                t.empty_drawn_in_world,
                t.refilled_before_submit,
                t.still_empty_at_submit
            );
        }
        let memory = self.memory.held();
        log_info!(
            "textures: %llu blocks of image memory made, %llu given back; making them took %.1f ms, the longest %.1f ms; %llu images had memory of their own",
            memory.made,
            memory.given_back,
            memory.making_nanos as f64 / 1e6,
            memory.longest_nanos as f64 / 1e6,
            memory.alone_made
        );
        log_info!(
            "textures: %llu blocks started ahead, %llu blocks taken from ahead (%llu of them waited for), %llu empty blocks kept for the next; %llu made at a draw, in %.1f ms",
            memory.made_ahead,
            memory.taken_ahead,
            memory.waited_ahead,
            memory.kept,
            memory.made_now,
            memory.made_now_nanos as f64 / 1e6
        );
        log_info!(
            "textures: %llu MB held at the end, against a budget of %llu MB; %llu evicted (%llu MB) in %llu passes",
            self.live_bytes >> 20,
            self.budget >> 20,
            self.evicted,
            self.evicted_bytes >> 20,
            self.eviction_passes
        );
        log_info!(
            "textures: %llu released (%llu MB) once idle because their memory had been rewritten; %llu kept, a page of theirs written and their bytes not; %llu fetch constants were another sampler state of an image already held",
            self.stale,
            self.stale_bytes >> 20,
            self.stale_not_really,
            self.shared
        );
        if self.marked_empty != 0 {
            log_info!("textures: %llu binds painted magenta (MW2_MARK_EMPTY)", self.marked_empty);
        }
        if t.failures_made != 0 || t.failures_failed_again != 0 {
            log_info!(
                "textures: failed lookups made again the frame after: %llu succeeded, %llu failed again",
                t.failures_made,
                t.failures_failed_again
            );
        }
        // Printed whenever the count is taken, zero included, so a run shows the switch was on.
        #[cfg(feature = "diagnostics")]
        let counting = count_past_rows();
        #[cfg(not(feature = "diagnostics"))]
        let counting = false;
        if counting || t.changed_past_rows != 0 {
            log_info!(
                "textures: %llu changes caught only past each level's whole rows (a tiled slice under 4 KB, a 1D tail), which the watch and signature missed before they covered what is read",
                t.changed_past_rows
            );
        }
        #[cfg(feature = "diagnostics")]
        if self.let_go.count != 0 {
            log_info!(
                "textures: %llu idle images let go, the youngest %llu ms after its last use's submission began and %llu frames after its last bind",
                self.let_go.count,
                self.let_go.nanoseconds / 1_000_000,
                self.let_go.frames
            );
        }
    }
}

/// vk::textures::ForgetViews, as the resolves call it (renderer_resolve.rs, making a
/// resolve image with the device idle).
pub fn forget_views(cache: &mut TextureCache) {
    cache.forget_views();
}

/// What TexturesFor asks of the cache (renderer_draw.rs's textures_for), on the device the
/// cache uploads with.
pub struct Bound<'a> {
    pub cache: &'a mut TextureCache,
    pub vk: &'a Vk,
}

impl super::renderer_draw::Textures for Bound<'_> {
    #[inline]
    fn epoch(&self) -> u64 {
        self.cache.epoch()
    }
    #[inline]
    fn upload(&mut self, fetch: &TextureFetch) -> u64 {
        self.cache.upload(self.vk, fetch, None)
    }
    #[inline]
    fn adopt(&mut self, key: u64, view: vk::ImageView, fetch: &TextureFetch, filterable: bool) -> u64 {
        self.cache.adopt(self.vk, key, view, fetch, filterable)
    }
    #[inline]
    fn handle(&mut self, id: u64, kind: u8) -> u32 {
        self.cache.handle(id, kind)
    }
    #[inline]
    fn note_drawn_in_world(&mut self, id: u64) -> u64 {
        self.cache.note_drawn_in_world(self.vk, id)
    }
}
