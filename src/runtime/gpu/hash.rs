// The renderer's hashes.
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

/// A hash over a key made of plain words, for the maps looked up on the draw path.
/// `size` is the key's size in bytes (its first value) and `words` its bytes as
/// little-endian u64s in order, a tail shorter than eight zero-extended.
#[inline]
pub fn words_hash(size: usize, words: impl IntoIterator<Item = u64>) -> u64 {
    let mut hash = size as u64;
    for word in words {
        hash = (hash ^ word).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        hash ^= hash >> 29;
    }
    hash
}

/// A key whose every byte is part of its value (std::has_unique_object_representations),
/// as words_hash takes it.
pub trait Words {
    const SIZE: usize;
    fn words(&self) -> impl Iterator<Item = u64>;
}

/// The hasher of the maps keyed by Words: a key's Hash gives words_hash whole, through
/// write_u64, and this hands it back unchanged: words_hash is the hash.
#[derive(Default, Clone, Copy)]
pub struct WordsHasher(u64);

impl Hasher for WordsHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        // Only Words keys are hashed with this, and they come through write_u64; anything
        // else is words_hash over its bytes.
        let words = bytes.chunks(8).map(|chunk| {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            u64::from_le_bytes(word)
        });
        self.0 = words_hash(bytes.len(), words);
    }

    #[inline]
    fn write_u64(&mut self, hash: u64) {
        self.0 = hash;
    }
}

pub type WordsBuild = BuildHasherDefault<WordsHasher>;
