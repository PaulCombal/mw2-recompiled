#pragma once
// Which EDRAM surface the title draws its shadow maps in, so the renderer
// can draw them at a scale of their own (MW2_SHADOW_SCALE).
#include <cstdint>

extern "C"
{
    bool mw2_shadows_is_map_depth(uint32_t depthInfo, uint32_t samples);
    bool mw2_shadows_surfaces_known();
    uint32_t mw2_shadows_surface_height();
}

namespace shadows
{
    // Whether a pass whose RB_DEPTH_INFO is `depthInfo`, drawn with
    // `samples` (RB_SURFACE_INFO's sample count), draws into the depth
    // surface the title made for its sun or spot shadow maps.
    inline bool IsShadowMapDepth(uint32_t depthInfo, uint32_t samples) { return mw2_shadows_is_map_depth(depthInfo, samples); }
    // Whether the title has made those surfaces yet: until it has, every
    // pass is answered no, and the answer may change.
    inline bool SurfacesKnown() { return mw2_shadows_surfaces_known(); }
    // The taller of the two, in the title's rows: 2048.
    inline uint32_t SurfaceHeight() { return mw2_shadows_surface_height(); }
}
