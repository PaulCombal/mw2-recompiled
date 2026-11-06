// The shader translator: Xenos microcode to SPIR-V, with the SPIR-V writer,
// the microcode's fields and the binding numbers it is built against.
//
// What this folder writes is what the pipeline cache holds, so every file here
// is in the shader build id: build.rs's shader_build() and CMake's
// MW2_SHADER_BUILD_INPUTS list the same files, and a change here makes players'
// copies compile their shaders again. SPIR-V builder methods only built-in
// shaders use go in a file outside this folder for that reason. Until the
// launcher moves to Rust, the C++ translator (shader_translator.cpp, which the
// launcher compiles) is a second copy: a change goes into both, and
// tools/shader_diff/shader_diff.py must find them the same.
//
// Safe code throughout, and nothing shared between calls; the C interface the
// C++ renderer calls through is ffi.rs, the one file allowed `unsafe`.
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

// The C interface, for the C++ renderer's forwarders (shader_translator_rust.cpp).
#[cfg(mw2_owns_shader_translator)]
pub mod ffi;

pub use translator::{TranslationMeta, describe_parts, translate};
