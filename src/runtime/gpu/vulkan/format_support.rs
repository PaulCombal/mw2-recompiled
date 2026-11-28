// What the device can do with a format, and what the renderer does where it cannot. Each
// decision here reads a format's optimal-tiling features through `features`, which
// answers as the device does, less what MW2_FAKE_FORMAT_UNSUPPORTED takes away (a
// diagnostics switch: a comma-separated list of VkFormat numbers, each with every feature
// taken away, or with `:<hex>` only those VkFormatFeatureFlags bits -- 37:1000, say,
// leaves R8G8B8A8_UNORM unfilterable -- here and in Vk's format queries; not in
// the shaders crate's depth formats, which the launcher shares and the shader build's id
// hashes).
//
// The rules, each from Xenia's Vulkan texture cache (src/xenia/gpu/vulkan/
// vulkan_texture_cache.cc, VulkanTextureCache::Initialize and GetSamplerParameters), its
// render target cache (vulkan_render_target_cache.cc) and its presenter
// (src/xenia/ui/vulkan/vulkan_presenter.cc), and written down in docs/textures.md:
//
// - A texture's host format (texture_host): a block-compressed format, which Vulkan
//   leaves optional, is used where the device can sample it and filter it linearly, else
//   the texture is decompressed on upload -- DXT1, DXT2/3, DXT4/5 to R8G8B8A8, DXT5A to
//   R8, DXN to R8G8 (texture_convert.rs). A 16-bit unsigned normalised format, also
//   optional, is used where it can be sampled, else the texture is converted to the half
//   float format of as many channels: Xenia's choice of precision over filtering. 4_4_4_4
//   is R4G4B4A4, optional, where it can be sampled and filtered, else Xenia's B4G4R4A4,
//   which every device has, with red and blue swapped on upload. Any other format that
//   cannot be sampled is refused ("device cannot sample this format").
// - A sampler asks for linear filtering only of an image whose format can be filtered:
//   otherwise its minification, magnification and mip filters are point and it has no
//   anisotropy (texture_cache.rs's point_only). Resolve copies the same.
// - A render target whose format cannot be drawn into is refused like one of no format; a
//   colour target is made sampled only where its format can be, since nothing samples a
//   colour target (resolves copy out of it). A resolve copy, which textures sample, is
//   made only of a format that can be sampled, and its sRGB view only where the sRGB
//   format can be.
// - A blit (the swap's, into the present image, and the presenter's, into the swapchain)
//   filters linearly where its source format can be filtered, else takes the nearest
//   texel; a source that cannot be a blit source is not blitted.
// - The swapchain: Xenia's choice of surface format (B8G8R8A8_UNORM, then R8G8B8A8_UNORM,
//   each in the sRGB colour space before another, then any format in that colour space,
//   then the first), among the formats that are not sRGB-encoded and can be blitted into;
//   an sRGB-encoded one only where no other can be blitted into, said once. No swapchain
//   where the surface cannot be a transfer destination or offers no format that can be
//   blitted into.
// - Depth (shaders/src/shader_build.rs, which the launcher shares, unchanged): D24S8 where it can be
//   drawn into and sampled, else D32S8 where it can be drawn into (Xenia takes D32S8
//   unchecked); sampling is asked of each use instead -- a depth resolve's copy (above)
//   and the drawn depth resolve's view.
#![deny(unsafe_op_in_unsafe_fn)]

use super::texture_convert::Convert;
#[cfg(feature = "diagnostics")]
use crate::log::log_info;
use ash::vk;

/// MW2_FAKE_FORMAT_UNSUPPORTED's formats and the features each loses, read once; none
/// without diagnostics.
#[cfg(feature = "diagnostics")]
fn faked() -> &'static [(i32, u32)] {
    static FAKED: std::sync::OnceLock<Vec<(i32, u32)>> = std::sync::OnceLock::new();
    FAKED.get_or_init(|| {
        let Some(text) = crate::env::text(c"MW2_FAKE_FORMAT_UNSUPPORTED") else { return Vec::new() };
        let list: Vec<(i32, u32)> = String::from_utf8_lossy(text.to_bytes())
            .split(',')
            .filter_map(|entry| {
                let (format, mask) = entry.trim().split_once(':').unwrap_or((entry.trim(), "ffffffff"));
                Some((format.parse::<i32>().ok()?, u32::from_str_radix(mask, 16).ok()?))
            })
            .collect();
        log_info!("renderer: MW2_FAKE_FORMAT_UNSUPPORTED: features taken away: %s", text);
        list
    })
}

/// The features MW2_FAKE_FORMAT_UNSUPPORTED takes away from the format.
#[inline]
pub fn faked_away(format: vk::Format) -> vk::FormatFeatureFlags {
    #[cfg(feature = "diagnostics")]
    {
        let mask = faked().iter().filter(|(f, _)| *f == format.as_raw()).fold(0, |all, (_, mask)| all | mask);
        vk::FormatFeatureFlags::from_raw(mask)
    }
    #[cfg(not(feature = "diagnostics"))]
    {
        let _ = format;
        vk::FormatFeatureFlags::empty()
    }
}

/// The format's optimal-tiling features, as the decisions here read them.
///
/// # Safety
/// `physical` is a live physical device of `instance`.
pub unsafe fn features(
    instance: &ash::InstanceFnV1_0,
    physical: vk::PhysicalDevice,
    format: vk::Format,
) -> vk::FormatFeatureFlags {
    {
        let mut out = vk::FormatProperties::default();
        // SAFETY: the caller's.
        unsafe { (instance.get_physical_device_format_properties)(physical, format, &mut out) };
        out.optimal_tiling_features & !faked_away(format)
    }
}

const SAMPLED: vk::FormatFeatureFlags = vk::FormatFeatureFlags::SAMPLED_IMAGE;
const FILTERED: vk::FormatFeatureFlags = vk::FormatFeatureFlags::from_raw(
    vk::FormatFeatureFlags::SAMPLED_IMAGE.as_raw() | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR.as_raw(),
);

/// Whether an image of these features may be sampled with a linear filter.
#[inline]
pub fn filterable(features: vk::FormatFeatureFlags) -> bool {
    features.contains(FILTERED)
}

/// A texture's host format and how its bytes get there, for the guest format whose
/// direct equivalent is `direct` (texture_formats.rs's vulkan_format_for).
pub fn texture_host(direct: i32, features: impl Fn(vk::Format) -> vk::FormatFeatureFlags) -> (i32, Convert) {
    let has = |wanted: vk::FormatFeatureFlags| features(vk::Format::from_raw(direct)).contains(wanted);
    let fallback = match vk::Format::from_raw(direct) {
        vk::Format::BC1_RGBA_UNORM_BLOCK => (vk::Format::R8G8B8A8_UNORM, Convert::Bc1, FILTERED),
        vk::Format::BC2_UNORM_BLOCK => (vk::Format::R8G8B8A8_UNORM, Convert::Bc2, FILTERED),
        vk::Format::BC3_UNORM_BLOCK => (vk::Format::R8G8B8A8_UNORM, Convert::Bc3, FILTERED),
        vk::Format::BC4_UNORM_BLOCK => (vk::Format::R8_UNORM, Convert::Bc4, FILTERED),
        vk::Format::BC5_UNORM_BLOCK => (vk::Format::R8G8_UNORM, Convert::Bc5, FILTERED),
        vk::Format::R16_UNORM => (vk::Format::R16_SFLOAT, Convert::Unorm16ToHalf, SAMPLED),
        vk::Format::R16G16_UNORM => (vk::Format::R16G16_SFLOAT, Convert::Unorm16ToHalf, SAMPLED),
        vk::Format::R16G16B16A16_UNORM => (vk::Format::R16G16B16A16_SFLOAT, Convert::Unorm16ToHalf, SAMPLED),
        vk::Format::R4G4B4A4_UNORM_PACK16 => (vk::Format::B4G4R4A4_UNORM_PACK16, Convert::Rgba4ToBgra4, FILTERED),
        _ => return (direct, Convert::None),
    };
    let (format, how, wanted) = fallback;
    if has(wanted) { (direct, Convert::None) } else { (format.as_raw(), how) }
}

/// The filter of a blit out of an image of these features; None when it cannot be blitted
/// out of.
#[inline]
pub fn blit_filter(source: vk::FormatFeatureFlags) -> Option<vk::Filter> {
    if !source.contains(vk::FormatFeatureFlags::BLIT_SRC) {
        return None;
    }
    Some(if source.contains(vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR) {
        vk::Filter::LINEAR
    } else {
        vk::Filter::NEAREST
    })
}

/// Whether the format is sRGB-encoded: written through it, a picture is encoded again.
pub fn srgb_encoded(format: vk::Format) -> bool {
    use vk::Format as F;
    matches!(
        format,
        F::R8_SRGB
            | F::R8G8_SRGB
            | F::R8G8B8_SRGB
            | F::B8G8R8_SRGB
            | F::R8G8B8A8_SRGB
            | F::B8G8R8A8_SRGB
            | F::A8B8G8R8_SRGB_PACK32
            | F::BC1_RGB_SRGB_BLOCK
            | F::BC1_RGBA_SRGB_BLOCK
            | F::BC2_SRGB_BLOCK
            | F::BC3_SRGB_BLOCK
            | F::BC7_SRGB_BLOCK
            | F::ETC2_R8G8B8_SRGB_BLOCK
            | F::ETC2_R8G8B8A1_SRGB_BLOCK
            | F::ETC2_R8G8B8A8_SRGB_BLOCK
    ) || (F::ASTC_4X4_SRGB_BLOCK.as_raw()..=F::ASTC_12X12_SRGB_BLOCK.as_raw()).contains(&format.as_raw())
        && format.as_raw() % 2 == 0
}

/// Xenia's choice of surface format among `formats` (VulkanPresenter's), the ones that are
/// not sRGB-encoded and can be blitted into first; the bool says an sRGB-encoded one had
/// to be taken. None when none can be blitted into.
pub fn choose_surface_format(
    formats: &[vk::SurfaceFormatKHR],
    blit_dst: impl Fn(vk::Format) -> bool,
) -> Option<(vk::SurfaceFormatKHR, bool)> {
    // Only UNDEFINED: any format will do (the Vulkan specification's reading; Xenia's
    // condition also takes any lone format for UNDEFINED, which a surface does not allow).
    if formats.len() == 1 && formats[0].format == vk::Format::UNDEFINED {
        let any =
            vk::SurfaceFormatKHR { format: vk::Format::B8G8R8A8_UNORM, color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR };
        return blit_dst(any.format).then_some((any, false));
    }
    let pick = |allow_srgb: bool| -> Option<vk::SurfaceFormatKHR> {
        let eligible = |f: &vk::SurfaceFormatKHR| blit_dst(f.format) && (allow_srgb || !srgb_encoded(f.format));
        let srgb_space = |f: &vk::SurfaceFormatKHR| f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR;
        let (mut primary, mut secondary, mut other_srgb) = (None, None, None);
        for f in formats.iter().filter(|f| eligible(f)) {
            let slot = match f.format {
                vk::Format::B8G8R8A8_UNORM => &mut primary,
                vk::Format::R8G8B8A8_UNORM => &mut secondary,
                _ => {
                    if srgb_space(f) && other_srgb.is_none() {
                        other_srgb = Some(*f);
                    }
                    continue;
                }
            };
            match slot {
                None => *slot = Some(*f),
                Some(seen) if srgb_space(f) && !srgb_space(seen) => *slot = Some(*f),
                _ => {}
            }
        }
        match (primary, secondary) {
            (Some(p), Some(s)) => Some(if srgb_space(&p) || !srgb_space(&s) { p } else { s }),
            (Some(p), None) => Some(p),
            (None, Some(s)) => Some(s),
            (None, None) => other_srgb.or_else(|| formats.iter().copied().find(|f| eligible(f))),
        }
    };
    if let Some(chosen) = pick(false) {
        return Some((chosen, false));
    }
    pick(true).map(|chosen| (chosen, true))
}
