// The C interface the C++ renderer translates through
// (runtime/gpu/vulkan/shader_translator_rust.cpp forwards shader::Translate
// and shader::Describe here). What varies in size goes out through a sink the
// caller passes, so the caller's own containers hold it and nothing is freed
// across the boundary.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_void};

use super::{TranslationMeta, describe_parts, translate};

pub type WordSink = extern "C" fn(context: *mut c_void, words: *const u32, count: usize);
pub type TextSink = extern "C" fn(context: *mut c_void, text: *const c_char, length: usize);

/// Translates `count` words of microcode in host byte order. Writes every
/// metadata field to `*meta`, hands the SPIR-V to `spirv_sink` and the error
/// text (empty when it succeeded) to `error_sink`, each exactly once, and
/// returns whether it succeeded.
///
/// # Safety
///
/// `words` must point at `count` readable words (or be null with `count` 0),
/// and `meta` at a writable TranslationMeta or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mw2_shader_translate(
    pixel: bool,
    words: *const u32,
    count: usize,
    scaled: bool,
    meta: *mut TranslationMeta,
    spirv_context: *mut c_void,
    spirv_sink: WordSink,
    error_context: *mut c_void,
    error_sink: TextSink,
) -> bool {
    let program: &[u32] = if words.is_null() || count == 0 {
        &[]
    } else {
        // SAFETY: the caller hands `count` readable words, which it keeps
        // alive and unchanged for the call.
        unsafe { std::slice::from_raw_parts(words, count) }
    };
    let result = translate(pixel, program, scaled);
    if !meta.is_null() {
        // SAFETY: the caller hands a writable TranslationMeta.
        unsafe { meta.write(result.meta) };
    }
    spirv_sink(spirv_context, result.spirv.as_ptr(), result.spirv.len());
    error_sink(error_context, result.error.as_ptr().cast::<c_char>(), result.error.len());
    result.ok
}

/// shader::Describe of a translation, from its parts: whether it succeeded,
/// its error text (`error_length` bytes, no terminator needed), how many
/// SPIR-V words it made and its metadata. The text goes to `text_sink` once.
///
/// # Safety
///
/// `error` must point at `error_length` readable bytes (or be null with
/// `error_length` 0), and `meta` at a readable TranslationMeta or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mw2_shader_describe(
    ok: bool,
    error: *const c_char,
    error_length: usize,
    spirv_words: usize,
    meta: *const TranslationMeta,
    text_context: *mut c_void,
    text_sink: TextSink,
) {
    let error_bytes: &[u8] = if error.is_null() || error_length == 0 {
        &[]
    } else {
        // SAFETY: the caller hands `error_length` readable bytes.
        unsafe { std::slice::from_raw_parts(error.cast::<u8>(), error_length) }
    };
    let fields = if meta.is_null() {
        TranslationMeta::default()
    } else {
        // SAFETY: the caller hands a readable TranslationMeta.
        unsafe { meta.read() }
    };
    // Bytes in, bytes out, as the C++'s snprintf: nothing is read as UTF-8.
    let text = describe_parts(ok, error_bytes, spirv_words, &fields);
    text_sink(text_context, text.as_ptr().cast::<c_char>(), text.len());
}
