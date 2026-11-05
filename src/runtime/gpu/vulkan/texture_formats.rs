// Xenos texture formats as Vulkan ones (runtime/gpu/vulkan/texture_formats.h). The
// VkFormat values are plain numbers here, so this needs no Vulkan and the rigs no
// device.

const UNDEFINED: i32 = 0;
const R4G4B4A4_UNORM_PACK16: i32 = 2;
const R5G6B5_UNORM_PACK16: i32 = 4;
const R8_UNORM: i32 = 9;
const R8G8_UNORM: i32 = 16;
const R8G8B8A8_UNORM: i32 = 37;
const R8G8B8A8_SRGB: i32 = 43;
const A2B10G10R10_UNORM_PACK32: i32 = 64;
const R16_UNORM: i32 = 70;
const R16_SFLOAT: i32 = 76;
const R16G16_UNORM: i32 = 77;
const R16G16_SFLOAT: i32 = 83;
const R16G16B16A16_UNORM: i32 = 91;
const R16G16B16A16_SFLOAT: i32 = 97;
const R32_SFLOAT: i32 = 100;
const R32G32_SFLOAT: i32 = 103;
const R32G32B32A32_SFLOAT: i32 = 109;
const BC1_RGBA_UNORM_BLOCK: i32 = 133;
const BC1_RGBA_SRGB_BLOCK: i32 = 134;
const BC2_UNORM_BLOCK: i32 = 135;
const BC2_SRGB_BLOCK: i32 = 136;
const BC3_UNORM_BLOCK: i32 = 137;
const BC3_SRGB_BLOCK: i32 = 138;
const BC4_UNORM_BLOCK: i32 = 139;
const BC5_UNORM_BLOCK: i32 = 141;

/// Or UNDEFINED (0) for one with no direct equivalent. The block-compressed formats
/// go across untouched -- BC1/2/3 are DXT1/2-3/4-5 byte for byte, BC4 is DXT5A -- so
/// for those the untiler's output is the upload.
pub fn vulkan_format_for(format: u32) -> i32 {
    match format {
        2 | 8 | 9 => R8_UNORM,         // 8, 8_A, 8_B
        4 => R5G6B5_UNORM_PACK16,      // 5_6_5
        6 | 14 | 50 => R8G8B8A8_UNORM, // 8_8_8_8
        7 | 54 => A2B10G10R10_UNORM_PACK32,
        10 => R8G8_UNORM, // 8_8
        15 => R4G4B4A4_UNORM_PACK16,
        18 | 51 => BC1_RGBA_UNORM_BLOCK, // DXT1
        19 | 52 => BC2_UNORM_BLOCK,      // DXT2_3
        20 | 53 => BC3_UNORM_BLOCK,      // DXT4_5
        24 => R16_UNORM,
        25 => R16G16_UNORM,
        26 => R16G16B16A16_UNORM,
        30 => R16_SFLOAT,
        31 => R16G16_SFLOAT,
        32 => R16G16B16A16_SFLOAT,
        36 => R32_SFLOAT,
        37 => R32G32_SFLOAT,
        38 => R32G32B32A32_SFLOAT,
        49 => BC5_UNORM_BLOCK, // DXN
        // DXT3A is unpacked to one byte a texel by read_texture; only DXT5A is BC4.
        58 => R8_UNORM,
        59 => BC4_UNORM_BLOCK, // DXT5A
        // DXT3A_AS_1_1_1_1 (61) is explicit four-bit values like DXT3A, each read as
        // four one-bit channels; not BC4, and not used by MW2. CTX1 (60) stores two
        // 8-bit channels the way DXN stores one pair, but not in a layout any BC
        // format matches; it would have to be expanded first.
        _ => UNDEFINED,
    }
}

/// Where a guest format's channels land once it has been given to a host format with
/// more of them: twelve bits, three to a component, 0-3 a channel, 4 zero, 5 one.
pub fn host_swizzle_for(format: u32) -> u32 {
    // Three bits a component, low component first: 0 R, 1 G, 2 B, 3 A.
    const RRRR: u32 = 0;
    const RGGG: u32 = 1 << 3 | 1 << 6 | 1 << 9;
    const RGBB: u32 = 1 << 3 | 2 << 6 | 2 << 9;
    const RGBA: u32 = 1 << 3 | 2 << 6 | 3 << 9;
    match format {
        // One channel, answered in all four.
        0 | 1 | 2 | 8 | 9 | 22 | 23 | 24 | 27 | 30 | 33 | 36 | 39 | 41 | 43 | 44 | 46 | 47 => RRRR,
        // Two, the second answering blue and alpha as well.
        10 | 13 | 25 | 28 | 31 | 34 | 37 | 40 | 42 | 45 | 48 | 60 => RGGG,
        // DXN is a pair of channels in a host BC5, and the title reads its Y from
        // alpha -- a flat host one there points every normal map's normal at the
        // camera, which puts the sun behind every surface it lights.
        49 => RGGG,
        // Three, the third answering alpha.
        4 | 11 | 12 | 16 | 17 | 55 | 56 | 57 => RGBB,
        // DXT3A and DXT5A are a single channel however they are unpacked.
        58 | 59 => RRRR,
        _ => RGBA,
    }
}

/// The fetch constant's swizzle read through the host format's.
pub fn compose_swizzle(guest: u32, host_format: u32) -> u32 {
    let mut out = 0;
    for i in 0..4 {
        let want = (guest >> (3 * i)) & 7;
        // 4 and 5 are the literals zero and one, and name no channel to look up; 6
        // and 7 are not defined, and are folded onto them rather than handed to a
        // driver.
        let got = if want >= 4 { want & 5 } else { (host_format >> (3 * want)) & 7 };
        out |= got << (3 * i);
    }
    out
}

/// The sRGB counterpart of a colour format, or UNDEFINED. Xenos gamma is a piecewise
/// curve rather than sRGB exactly, but the two agree to within a code value over the
/// range that matters, and a view in the sRGB format costs nothing where a shader
/// would cost a fetch.
pub fn srgb_format_for(format: i32) -> i32 {
    match format {
        R8G8B8A8_UNORM => R8G8B8A8_SRGB,
        BC1_RGBA_UNORM_BLOCK => BC1_RGBA_SRGB_BLOCK,
        BC2_UNORM_BLOCK => BC2_SRGB_BLOCK,
        BC3_UNORM_BLOCK => BC3_SRGB_BLOCK,
        _ => UNDEFINED,
    }
}

pub fn vulkan_format_name(format: i32) -> &'static std::ffi::CStr {
    match format {
        R8_UNORM => c"R8",
        R8G8_UNORM => c"R8G8",
        R8G8B8A8_UNORM => c"R8G8B8A8",
        R5G6B5_UNORM_PACK16 => c"R5G6B5",
        R4G4B4A4_UNORM_PACK16 => c"R4G4B4A4",
        A2B10G10R10_UNORM_PACK32 => c"A2B10G10R10",
        R16_UNORM => c"R16",
        R16G16_UNORM => c"R16G16",
        R16G16B16A16_UNORM => c"R16G16B16A16",
        R16_SFLOAT => c"R16F",
        R16G16_SFLOAT => c"R16G16F",
        R16G16B16A16_SFLOAT => c"R16G16B16A16F",
        R32_SFLOAT => c"R32F",
        R32G32_SFLOAT => c"R32G32F",
        R32G32B32A32_SFLOAT => c"R32G32B32A32F",
        BC1_RGBA_UNORM_BLOCK => c"BC1",
        BC2_UNORM_BLOCK => c"BC2",
        BC3_UNORM_BLOCK => c"BC3",
        BC4_UNORM_BLOCK => c"BC4",
        BC5_UNORM_BLOCK => c"BC5",
        _ => c"unsupported",
    }
}
