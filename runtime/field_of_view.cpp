// MW2_FOV: the field of view, as the angle the title's own cg_fov counts --
// the width of a 4:3 picture, 65 on the console. The 16:9 picture is as tall
// as the 4:3 one and wider, so 65 is 80.7 degrees across it.
//
// The title has two settings for it and both belong to its scripts: cg_fov is
// put back to 65 at every multiplayer spawn and walked through other angles
// by the campaign's climbs and cinematics, and cg_fovScale is written at every
// level load and multiplayer connect (0.75 in split screen, 1 otherwise). A
// value given to either is gone at the next script that writes it. So neither
// is touched: the title is made to see cg_fovScale as what the scripts last
// wrote times MW2_FOV / 65, in the two functions that read it.
//
//   T_CG_ViewFov   the angle a client's view is drawn with: cg_fov, or the
//                  weapon's while aiming, or a turret's or a kill camera's,
//                  times cg_fovScale, held between cg_fovMin and 170.
//   T_CG_CullZoom  the zoom the distance culls take from that view, divided by
//                  cg_fovScale so the scale does not bring them nearer.
//
// Each hook scales the function's result, which is the result it would have
// had with the scale in the setting, except for a view already held at one of
// the two bounds before the scale: that one is scaled from the bound.
#include <ppc_recomp_shared.h>
#include "title.h"
#include "env.h"
#include "log.h"

#include <algorithm>

namespace
{
    constexpr double kConsole = 65.0;       // cg_fov's default
    constexpr double kWidest = 120.0;
    constexpr double kTitleMost = 170.0;    // T_CG_ViewFov's own upper bound

    double Scale()
    {
        static const double scale = [] {
            const double asked = env::Real("MW2_FOV", kConsole);
            const double fov = std::clamp(asked, kConsole, kWidest);
            if (fov != asked) LOGW("fov: MW2_FOV is %g to %g, not %g", kConsole, kWidest, asked);
            if (fov != kConsole) LOGI("fov: the view is %g degrees where the console's is %g", fov, kConsole);
            return fov / kConsole;
        }();
        return scale;
    }
}

GUEST_HOOK(T_CG_ViewFov)
{
    GUEST_ORIG(T_CG_ViewFov)(ctx, base);
    ctx.f1.f64 = double(float(std::min(ctx.f1.f64 * Scale(), kTitleMost)));
}

GUEST_HOOK(T_CG_CullZoom)
{
    GUEST_ORIG(T_CG_CullZoom)(ctx, base);
    ctx.f1.f64 = double(float(ctx.f1.f64 / Scale()));
}
