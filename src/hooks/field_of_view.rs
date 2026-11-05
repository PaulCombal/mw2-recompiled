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

use crate::env;
use crate::guest::{Register, guest_hook};
use crate::log::{log_info, log_warn};
use crate::title::{T_CG_CullZoom, T_CG_ViewFov};
use std::sync::LazyLock;

const CONSOLE: f64 = 65.0; // cg_fov's default
const WIDEST: f64 = 120.0;
const TITLE_MOST: f64 = 170.0; // T_CG_ViewFov's own upper bound

// Read at the first view drawn, in whatever rounding mode the guest then has.
static SCALE: LazyLock<f64> = LazyLock::new(|| {
    let asked = env::real(c"MW2_FOV", CONSOLE);
    // A NaN stays one, so the check below reports it.
    let fov = asked.clamp(CONSOLE, WIDEST);
    if fov != asked {
        log_warn!("fov: MW2_FOV is %g to %g, not %g", CONSOLE, WIDEST, asked);
    }
    if fov != CONSOLE {
        log_info!("fov: the view is %g degrees where the console's is %g", fov, CONSOLE);
    }
    fov / CONSOLE
});

guest_hook!(T_CG_ViewFov, |ctx, memory| {
    T_CG_ViewFov.original(ctx, memory);
    let scaled = ctx.f1.as_f64() * *SCALE;
    // std::min, not f64::min: this one hands a NaN on, as the C++ does.
    let held = if TITLE_MOST < scaled { TITLE_MOST } else { scaled };
    ctx.f1.f64 = f64::from(held as f32);
});

guest_hook!(T_CG_CullZoom, |ctx, memory| {
    T_CG_CullZoom.original(ctx, memory);
    ctx.f1.f64 = f64::from((ctx.f1.as_f64() / *SCALE) as f32);
});
