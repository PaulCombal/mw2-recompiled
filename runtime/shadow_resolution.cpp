// The title's shadow maps, by the surfaces it made for them.
//
// R_InitRenderTargets fills gfxRenderTargets, a render target a row of
// { GfxImage* image, D3DSurface* colour, D3DSurface* depth, width, height },
// in the order of the R_RENDERTARGET_ names: 11 is SHADOWMAP_LARGE, the sun's
// 1024x2048 (two cascades), 12 SHADOWMAP_SMALL, the spots' 512x2048. A
// D3DSurface holds the registers D3D writes when it is bound: +24 is
// RB_SURFACE_INFO (pitch, samples) and +28 RB_DEPTH_INFO (base tile, format)
// for a depth surface. Both shadow maps draw depth at tile 0 in D24FS8 at one
// sample, the sun's 1040 wide and the spots' 560. No other target the title
// made is there; a pass with no depth surface bound has RB_DEPTH_INFO 0, which
// is tile 0 in D24S8, another format.
//
// So a pass is a shadow map's when its depth registers name one of the two:
// the base tile, the format and the sample count. Not the pitch, which a
// depth surface does not have in the renderer (DepthKey).
#include "shadow_resolution.h"
#include "title.h"
#include "guest.h"

#include <algorithm>

namespace
{
    constexpr uint32_t kRowBytes = 20, kDepthSurface = 8, kHeight = 16;
    constexpr uint32_t kShadowMaps[] = { 11, 12 };
    constexpr uint32_t kSurfaceInfo = 24, kDepthInfo = 28;

    uint32_t Load(uint32_t address)
    {
        const uint8_t* p = guest::Base() + address;
        return (uint32_t(p[0]) << 24) | (uint32_t(p[1]) << 16) | (uint32_t(p[2]) << 8) | p[3];
    }

    // A depth surface's identity as the comparison needs it: the base tile,
    // the format bit and the sample count.
    uint32_t Identity(uint32_t depthInfo, uint32_t samples)
    {
        return (depthInfo & 0x10FFF) | ((samples & 3) << 20);
    }
}

bool shadows::IsShadowMapDepth(uint32_t depthInfo, uint32_t samples)
{
#ifdef T_DATA_RenderTargets
    const uint32_t asked = Identity(depthInfo, samples);
    for (uint32_t target : kShadowMaps)
    {
        // Null until R_InitRenderTargets has run, which is before any draw.
        const uint32_t surface = Load(T_DATA_RenderTargets + target * kRowBytes + kDepthSurface);
        if (!surface) continue;
        const uint32_t surfaceInfo = Load(surface + kSurfaceInfo);
        if (Identity(Load(surface + kDepthInfo), surfaceInfo >> 16) == asked) return true;
    }
#else
    (void)depthInfo;
    (void)samples;
#endif
    return false;
}

uint32_t shadows::SurfaceHeight()
{
    uint32_t tallest = 0;
#ifdef T_DATA_RenderTargets
    for (uint32_t target : kShadowMaps)
        tallest = std::max(tallest, Load(T_DATA_RenderTargets + target * kRowBytes + kHeight));
#endif
    return tallest ? tallest : 4096;
}

bool shadows::SurfacesKnown()
{
#ifdef T_DATA_RenderTargets
    for (uint32_t target : kShadowMaps)
        if (!Load(T_DATA_RenderTargets + target * kRowBytes + kDepthSurface)) return false;
    return true;
#else
    return false;
#endif
}
