// What the C++ renderer calls in the Rust one while the two share the game: each
// function is the C++ declaration of its name (runtime/rust_renderer.cpp forwards to
// it) and converts between C and the module's types and nothing else.
//
// Unsafe by nature: every pointer comes from the C++ forwarders, which pass a
// TextureFetch's six dwords, out-structs on their stack whose layouts both sides
// assert, the host addresses of guest ranges (read_texture's contract) and a callback
// that resizes a std::vector the forwarder owns and returns its zeroed bytes.
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(mw2_owns_texture)]
mod texture {
    use crate::runtime::gpu::texture::{self, Decoded, Extents, FormatInfo, Level, TextureFetch, TextureHeader};
    use std::ffi::{c_char, c_void};
    use std::ptr;

    type Reserve = unsafe extern "C" fn(context: *mut c_void, size: usize) -> *mut u8;

    /// # Safety
    /// `words` must point at six readable dwords.
    unsafe fn fetch_of(words: *const u32) -> TextureFetch {
        // SAFETY: the C++ passes TextureFetch::d, six dwords.
        TextureFetch { d: unsafe { *words.cast::<[u32; 6]>() } }
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_texture_read(
        fetch: *const u32,
        base: *const u8,
        mips: *const u8,
        out: *mut TextureHeader,
        reserve: Reserve,
        context: *mut c_void,
    ) -> bool {
        // SAFETY: six dwords (the forwarder's TextureFetch::d).
        let fetch = unsafe { fetch_of(fetch) };
        // SAFETY: base and mips are the texture cache's guest ranges; reserve resizes the
        // forwarder's vector (zero-filled) and returns its bytes, read_texture's contract.
        let header = unsafe { texture::read_texture(&fetch, base, mips, &mut |size| reserve(context, size)) };
        // SAFETY: `out` is the forwarder's TextureHeader, the same layout (asserted both sides).
        unsafe { out.write(header) };
        header.ok
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_texture_extents(fetch: *const u32, out: *mut Extents) {
        // SAFETY: six dwords, and the forwarder's TextureExtents (layout asserted both sides).
        unsafe { out.write(texture::extents_of(&fetch_of(fetch))) }
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_texture_format_of(format: u32, out: *mut FormatInfo) {
        // SAFETY: the forwarder's TextureFormatInfo (layout asserted both sides).
        unsafe { out.write(texture::texture_format_of(format)) }
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_texture_format_name(format: u32) -> *const c_char {
        texture::texture_format_name(format).map_or(ptr::null(), |name| name.as_ptr())
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_texture_tiled_offset_2d(x: i32, y: i32, pitch: u32, log2: u32) -> i32 {
        texture::tiled_offset_2d(x, y, pitch, log2)
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_texture_tiled_offset_3d(x: i32, y: i32, z: i32, pitch: u32, height: u32, log2: u32) -> i64 {
        texture::tiled_offset_3d(x, y, z, pitch, height, log2)
    }

    #[unsafe(no_mangle)]
    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn mw2_texture_decode_rgba(
        ok: bool,
        expanded: bool,
        bytes_per_block: u32,
        levels: *const Level,
        level_count: usize,
        bytes: *const u8,
        size: usize,
        format: u32,
        level: u32,
        rgba: Reserve,
        context: *mut c_void,
    ) -> bool {
        // SAFETY: the C++'s TextureData: its levels and its bytes.
        let data = unsafe {
            Decoded {
                ok,
                expanded,
                bytes_per_block,
                levels: if level_count == 0 { &[] } else { std::slice::from_raw_parts(levels, level_count) },
                bytes: if size == 0 { &[] } else { std::slice::from_raw_parts(bytes, size) },
            }
        };
        // SAFETY: rgba resizes the forwarder's vector (zero-filled) and returns its bytes.
        unsafe { texture::decode_to_rgba(&data, format, level, &mut |n| rgba(context, n)) }
    }
}

#[cfg(mw2_owns_texture_formats)]
mod formats {
    use crate::runtime::gpu::vulkan::texture_formats as f;
    use std::ffi::c_char;

    #[unsafe(no_mangle)]
    extern "C" fn mw2_formats_vulkan_format_for(format: u32) -> i32 {
        f::vulkan_format_for(format)
    }
    #[unsafe(no_mangle)]
    extern "C" fn mw2_formats_srgb_format_for(format: i32) -> i32 {
        f::srgb_format_for(format)
    }
    #[unsafe(no_mangle)]
    extern "C" fn mw2_formats_vulkan_format_name(format: i32) -> *const c_char {
        f::vulkan_format_name(format).as_ptr()
    }
    #[unsafe(no_mangle)]
    extern "C" fn mw2_formats_host_swizzle_for(format: u32) -> u32 {
        f::host_swizzle_for(format)
    }
    #[unsafe(no_mangle)]
    extern "C" fn mw2_formats_compose_swizzle(guest: u32, host: u32) -> u32 {
        f::compose_swizzle(guest, host)
    }
}
