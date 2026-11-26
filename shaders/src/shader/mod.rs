// The shader translator: Xenos microcode to SPIR-V, with the SPIR-V writer,
// the microcode's fields and the binding numbers it is built against.
//
// What this folder writes is what the pipeline cache holds, so every file here
// is in the shader build id (the crate's build.rs), and a change here makes
// players' copies compile their shaders again. SPIR-V builder methods only
// built-in shaders use go in the game, outside this crate, for that reason.
//
// Safe code throughout, and nothing shared between calls.
#![deny(unsafe_code)]

#[allow(dead_code)]
pub mod bindings;
#[allow(dead_code)]
pub mod spirv;
mod translator;
mod translator_alu;
mod translator_exports;
mod translator_fetch;
mod translator_flow;
#[allow(dead_code)]
pub mod ucode;

pub use translator::{Translation, TranslationMeta, describe_parts, translate};

/// The name a shader goes by: FNV-1a 64 over its microcode dwords, as the command
/// stream loads them. The game names what it draws with by it, and the launcher's
/// compile pass what it compiles (mw2-zone), so the two find the same shader.
#[inline]
pub fn hash(code: &[u32]) -> u64 {
    let mut hash: u64 = 1469598103934665603;
    for &word in code {
        hash ^= u64::from(word);
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}
