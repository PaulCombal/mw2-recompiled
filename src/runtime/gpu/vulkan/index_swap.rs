//! The index swap: a draw's big-endian indices byte-swapped into the frame arena, with the
//! lowest and highest index named on the way through.
//!
//! A list of nothing but restarts comes out lowest above highest, which write_indices turns
//! into vertex 0: the restart index counts as zero toward the highest, and as the largest
//! value it can never be the lowest. Nothing is read back from `out`, which is
//! write-combined memory.
//!
//! The AVX2 versions are written with intrinsics because the loop the compiler vectorised
//! for this x86-64-v2 build split every 32-byte store in two halves (its generic tuning
//! takes unaligned 32-byte accesses to be slow), and the stores into write-combined memory
//! were most of the swap's time. The arena aligns every allocation to at least 256 bytes,
//! so `out` is aligned and each store is one aligned 32-byte store; the source, the guest's
//! buffer, has any alignment. No file of the crate is used here, so a scratch crate can
//! hold these to the scalar loop.

use std::arch::x86_64::*;
use std::ptr;

/// The swap as a loop, for any CPU, and the reference the vector versions are held to.
macro_rules! scalar {
    ($name:ident, $t:ty) => {
        /// # Safety
        /// `count` indices readable at `source` (any alignment) and writable at `out`.
        #[inline(always)]
        pub unsafe fn $name(source: *const u8, out: *mut $t, count: u32) -> ($t, $t) {
            const RESTART: $t = <$t>::MAX;
            let (mut low, mut high): ($t, $t) = (RESTART, 0);
            let input = source.cast::<$t>();
            for i in 0..count as usize {
                // SAFETY: the caller's.
                let value = <$t>::from_be(unsafe { ptr::read_unaligned(input.add(i)) });
                // SAFETY: the caller's.
                unsafe { out.add(i).write_unaligned(value) };
                low = if value < low { value } else { low };
                let counted = if value == RESTART { 0 } else { value };
                high = if counted > high { counted } else { high };
            }
            (low, high)
        }
    };
}
scalar!(swap16_scalar, u16);
scalar!(swap32_scalar, u32);

/// x86-64-v2: the loop, which the compiler vectorises for SSE.
///
/// # Safety
/// `count` indices readable at `source` and writable at `out`.
pub unsafe fn swap16_v2(source: *const u8, out: *mut u16, count: u32) -> (u16, u16) {
    // SAFETY: the caller's.
    unsafe { swap16_scalar(source, out, count) }
}

/// # Safety
/// `count` indices readable at `source` and writable at `out`.
pub unsafe fn swap32_v2(source: *const u8, out: *mut u32, count: u32) -> (u32, u32) {
    // SAFETY: the caller's.
    unsafe { swap32_scalar(source, out, count) }
}

/// AVX2, 16 indices a step.
///
/// # Safety
/// `count` indices readable at `source` and writable at `out`, which is 32-byte aligned;
/// the CPU has AVX2.
#[target_feature(enable = "avx2")]
pub unsafe fn swap16_avx2(source: *const u8, out: *mut u16, count: u32) -> (u16, u16) {
    let steps = count as usize / 16;
    let swap = _mm256_setr_epi8(
        1, 0, 3, 2, 5, 4, 7, 6, 9, 8, 11, 10, 13, 12, 15, 14, 1, 0, 3, 2, 5, 4, 7, 6, 9, 8, 11, 10, 13, 12, 15, 14,
    );
    let restart = _mm256_set1_epi16(-1);
    let (mut low, mut low2) = (restart, restart);
    let (mut high, mut high2) = (_mm256_setzero_si256(), _mm256_setzero_si256());
    // Two steps at a time, each with its own lowest and highest, so that the two do not
    // wait on each other.
    let mut step = 0;
    while step + 2 <= steps {
        // SAFETY: (step + 2) * 16 <= count: inside both buffers; `out` is 32-byte aligned
        // (the caller's) and so is every 32 bytes after it.
        unsafe {
            let value = _mm256_shuffle_epi8(_mm256_loadu_si256(source.add(step * 32).cast()), swap);
            let value2 = _mm256_shuffle_epi8(_mm256_loadu_si256(source.add(step * 32 + 32).cast()), swap);
            _mm256_store_si256(out.add(step * 16).cast(), value);
            _mm256_store_si256(out.add(step * 16 + 16).cast(), value2);
            low = _mm256_min_epu16(low, value);
            low2 = _mm256_min_epu16(low2, value2);
            high = _mm256_max_epu16(high, _mm256_andnot_si256(_mm256_cmpeq_epi16(value, restart), value));
            high2 = _mm256_max_epu16(high2, _mm256_andnot_si256(_mm256_cmpeq_epi16(value2, restart), value2));
        }
        step += 2;
    }
    if step < steps {
        // SAFETY: as above, one step.
        unsafe {
            let value = _mm256_shuffle_epi8(_mm256_loadu_si256(source.add(step * 32).cast()), swap);
            _mm256_store_si256(out.add(step * 16).cast(), value);
            low = _mm256_min_epu16(low, value);
            high = _mm256_max_epu16(high, _mm256_andnot_si256(_mm256_cmpeq_epi16(value, restart), value));
        }
    }
    let low = _mm256_min_epu16(low, low2);
    let high = _mm256_max_epu16(high, high2);
    // The 16 lanes to one: the lowest by minpos, the highest as the lowest complement.
    let low128 = _mm_min_epu16(_mm256_castsi256_si128(low), _mm256_extracti128_si256(low, 1));
    let high128 = _mm_max_epu16(_mm256_castsi256_si128(high), _mm256_extracti128_si256(high, 1));
    let mut lowest = _mm_cvtsi128_si32(_mm_minpos_epu16(low128)) as u16;
    let all = _mm_set1_epi16(-1);
    let mut highest = !(_mm_cvtsi128_si32(_mm_minpos_epu16(_mm_xor_si128(high128, all))) as u16);
    let done = steps * 16;
    if done < count as usize {
        // SAFETY: the rest of both buffers.
        let (l, h) = unsafe { swap16_tail(source, out, count as usize, done) };
        lowest = lowest.min(l);
        highest = highest.max(h);
    }
    (lowest, highest)
}

/// AVX2, 8 indices a step.
///
/// # Safety
/// As swap16_avx2.
#[target_feature(enable = "avx2")]
pub unsafe fn swap32_avx2(source: *const u8, out: *mut u32, count: u32) -> (u32, u32) {
    let steps = count as usize / 8;
    let swap = _mm256_setr_epi8(
        3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12, 3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12,
    );
    let restart = _mm256_set1_epi32(-1);
    let mut low = restart;
    let mut high = _mm256_setzero_si256();
    for step in 0..steps {
        // SAFETY: step * 8 + 8 <= count; `out` 32-byte aligned (the caller's).
        unsafe {
            let raw = _mm256_loadu_si256(source.add(step * 32).cast());
            let value = _mm256_shuffle_epi8(raw, swap);
            _mm256_store_si256(out.add(step * 8).cast(), value);
            low = _mm256_min_epu32(low, value);
            high = _mm256_max_epu32(high, _mm256_andnot_si256(_mm256_cmpeq_epi32(value, restart), value));
        }
    }
    let mut lanes_low = [0u32; 8];
    let mut lanes_high = [0u32; 8];
    // SAFETY: two arrays of 32 bytes.
    unsafe {
        _mm256_storeu_si256(lanes_low.as_mut_ptr().cast(), low);
        _mm256_storeu_si256(lanes_high.as_mut_ptr().cast(), high);
    }
    let mut lowest = lanes_low.iter().copied().min().unwrap_or(u32::MAX);
    let mut highest = lanes_high.iter().copied().max().unwrap_or(0);
    let done = steps * 8;
    if done < count as usize {
        // SAFETY: the rest of both buffers.
        let (l, h) = unsafe { swap32_tail(source, out, count as usize, done) };
        lowest = lowest.min(l);
        highest = highest.max(h);
    }
    (lowest, highest)
}

/// The indices from `done` to `count`, fewer than one step: as one more step that ends at
/// `count` and overlaps the last, rewriting some with the values they already hold, where
/// there are a step's worth in all; the loop where there are not.
///
/// # Safety
/// `count` indices readable at `source` and writable at `out`; the CPU has AVX2.
#[target_feature(enable = "avx2")]
#[inline]
unsafe fn swap16_tail(source: *const u8, out: *mut u16, count: usize, done: usize) -> (u16, u16) {
    if count < 16 {
        // SAFETY: the caller's.
        return unsafe { swap16_scalar(source.add(done * 2), out.add(done), (count - done) as u32) };
    }
    let swap = _mm256_setr_epi8(
        1, 0, 3, 2, 5, 4, 7, 6, 9, 8, 11, 10, 13, 12, 15, 14, 1, 0, 3, 2, 5, 4, 7, 6, 9, 8, 11, 10, 13, 12, 15, 14,
    );
    let at = count - 16;
    // SAFETY: [at, count) inside both buffers; the store is unaligned.
    let value = unsafe {
        let value = _mm256_shuffle_epi8(_mm256_loadu_si256(source.add(at * 2).cast()), swap);
        _mm256_storeu_si256(out.add(at).cast(), value);
        value
    };
    let restart = _mm256_set1_epi16(-1);
    let counted = _mm256_andnot_si256(_mm256_cmpeq_epi16(value, restart), value);
    let low = _mm_min_epu16(_mm256_castsi256_si128(value), _mm256_extracti128_si256(value, 1));
    let high = _mm_max_epu16(_mm256_castsi256_si128(counted), _mm256_extracti128_si256(counted, 1));
    let lowest = _mm_cvtsi128_si32(_mm_minpos_epu16(low)) as u16;
    let highest = !(_mm_cvtsi128_si32(_mm_minpos_epu16(_mm_xor_si128(high, _mm_set1_epi16(-1)))) as u16);
    (lowest, highest)
}

/// As swap16_tail, eight 32-bit indices.
///
/// # Safety
/// As swap16_tail.
#[target_feature(enable = "avx2")]
#[inline]
unsafe fn swap32_tail(source: *const u8, out: *mut u32, count: usize, done: usize) -> (u32, u32) {
    if count < 8 {
        // SAFETY: the caller's.
        return unsafe { swap32_scalar(source.add(done * 4), out.add(done), (count - done) as u32) };
    }
    let swap = _mm256_setr_epi8(
        3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12, 3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12,
    );
    let at = count - 8;
    let mut lanes = [0u32; 8];
    // SAFETY: [at, count) inside both buffers; the stores are unaligned.
    unsafe {
        let value = _mm256_shuffle_epi8(_mm256_loadu_si256(source.add(at * 4).cast()), swap);
        _mm256_storeu_si256(out.add(at).cast(), value);
        _mm256_storeu_si256(lanes.as_mut_ptr().cast(), value);
    }
    let lowest = lanes.iter().copied().min().unwrap_or(u32::MAX);
    let highest = lanes.iter().copied().map(|v| if v == u32::MAX { 0 } else { v }).max().unwrap_or(0);
    (lowest, highest)
}
