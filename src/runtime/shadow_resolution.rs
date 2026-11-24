// The title's shadow maps, by the surfaces it made for them, so the renderer can
// draw them at a scale of their own (MW2_SHADOW_SCALE).
//
// R_InitRenderTargets fills gfxRenderTargets, a render target a row of
// { GfxImage* image, D3DSurface* colour, D3DSurface* depth, width, height }, in the
// order of the R_RENDERTARGET_ names: 11 is SHADOWMAP_LARGE, the sun's 1024x2048
// (two cascades), 12 SHADOWMAP_SMALL, the spots' 512x2048. A D3DSurface holds the
// registers D3D writes when it is bound: +24 is RB_SURFACE_INFO (pitch, samples) and
// +28 RB_DEPTH_INFO (base tile, format) for a depth surface. Both shadow maps draw
// depth at tile 0 in D24FS8 at one sample, the sun's 1040 wide and the spots' 560.
// No other target the title made is there; a pass with no depth surface bound has
// RB_DEPTH_INFO 0, which is tile 0 in D24S8, another format.
//
// So a pass is a shadow map's when its depth registers name one of the two: the base
// tile, the format and the sample count. Not the pitch, which a depth surface does
// not have in the renderer (DepthKey).

use crate::runtime::guest_memory;
use crate::title::*;

const ROW_BYTES: u32 = 20;
const DEPTH_SURFACE: u32 = 8;
const HEIGHT: u32 = 16;
const SHADOW_MAPS: [u32; 2] = [11, 12];
const SURFACE_INFO: u32 = 24;
const DEPTH_INFO: u32 = 28;

fn row(target: u32, field: u32) -> u32 {
    guest_memory::memory().u32_at(T_DATA_RenderTargets.wrapping_add(target.wrapping_mul(ROW_BYTES)).wrapping_add(field))
}

// A depth surface's identity as the comparison needs it: the base tile, the format
// bit and the sample count.
fn identity(depth_info: u32, samples: u32) -> u32 {
    (depth_info & 0x10FFF) | ((samples & 3) << 20)
}

/// Whether a pass whose RB_DEPTH_INFO is `depth_info`, drawn with `samples`
/// (RB_SURFACE_INFO's sample count), draws into the depth surface the title made for
/// its sun or spot shadow maps.
pub fn is_shadow_map_depth(depth_info: u32, samples: u32) -> bool {
    let memory = guest_memory::memory();
    let asked = identity(depth_info, samples);
    SHADOW_MAPS.iter().any(|&target| {
        // Null until R_InitRenderTargets has run, which is before any draw.
        let surface = row(target, DEPTH_SURFACE);
        surface != 0
            && identity(
                memory.u32_at(surface.wrapping_add(DEPTH_INFO)),
                memory.u32_at(surface.wrapping_add(SURFACE_INFO)) >> 16,
            ) == asked
    })
}

/// Whether the title has made those surfaces yet: until it has, every pass is
/// answered no, and the answer may change.
pub fn surfaces_known() -> bool {
    SHADOW_MAPS.iter().all(|&target| row(target, DEPTH_SURFACE) != 0)
}

/// The taller of the two, in the title's rows: 2048.
pub fn surface_height() -> u32 {
    match SHADOW_MAPS.iter().map(|&target| row(target, HEIGHT)).max() {
        Some(tallest) if tallest != 0 => tallest,
        _ => 4096,
    }
}
