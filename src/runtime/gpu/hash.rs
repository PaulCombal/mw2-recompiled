// The renderer's hashes (runtime/gpu/internal.h's company: renderer.h's ShaderHash).
//
// Never SipHash on a hot map: a key that is already unique -- the image-load memo's,
// a shader's hash -- needs only spreading, which one multiply and a rotate do. std's map
// takes its bucket from the hash's low bits, and a product's low bits are only its key's:
// the memo's key keeps the address above bit 17, so without the rotate every shader of
// one length would start its probe in the same group.

use std::hash::{BuildHasherDefault, Hasher};

/// The name a shader goes by: FNV-1a 64 over its microcode dwords, as the command
/// stream loads them (vk::renderer::ShaderHash).
#[inline]
pub fn shader_hash(code: &[u32]) -> u64 {
    let mut hash: u64 = 1469598103934665603;
    for &word in code {
        hash ^= u64::from(word);
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

/// For keys already unique: `(k * 0x9E3779B97F4A7C15).rotate_left(26)`, the product's
/// high bits brought down to the low ones the bucket is taken from.
#[derive(Default, Clone, Copy)]
pub struct MulHasher(u64);

impl Hasher for MulHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0.rotate_left(26)
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        // Only u64 keys are hashed with this; anything else folds in a byte at a time.
        for &byte in bytes {
            self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
    }

    #[inline]
    fn write_u64(&mut self, k: u64) {
        self.0 = k.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

pub type MulBuild = BuildHasherDefault<MulHasher>;
