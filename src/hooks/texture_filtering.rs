// MW2_ANISOTROPY=<2, 4, 8 or 16>: the anisotropic filtering every texture
// that has a mip chain is sampled with at least, where the console's is what
// each material asks for, 1:1 to 4:1.
//
// The title has the PC game's two settings for it, both registered by
// R_RegisterDvars with Dvar_RegisterInt: r_texFilterAnisoMax, 16, and
// r_texFilterAnisoMin, 1 to 16 and 1 by default, which the PC's ANISOTROPIC
// FILTERING slider sets. Its sampler states take the anisotropy of a
// material's sampler between the two; the ones that filter without mips (the
// menus, the HUD) take none whatever the setting. The minimum has no flags:
// it is no cheat, and nothing the title runs sets it. So it is registered
// with the setting as its default, which is the value it has from then on,
// as a PC player's saved one is.
//
// Without the switch, or with 1, nothing changes.

use crate::env;
use crate::guest::{Register, guest_hook};
use crate::log::{log_info, log_warn};
use crate::title::T_Dvar_RegisterInt;
use std::sync::LazyLock;

const CONSOLE: u32 = 1;
const MOST: u32 = 16;

// Read when the title first registers the setting, and not before.
static ASKED: LazyLock<u32> = LazyLock::new(|| {
    let asked = env::number(c"MW2_ANISOTROPY", CONSOLE.into());
    match asked {
        1 | 2 | 4 | 8 | 16 => {
            if asked != u64::from(CONSOLE) {
                log_info!("anisotropy: textures are filtered at %llux at least", asked);
            }
            asked as u32
        }
        _ => {
            log_warn!("anisotropy: MW2_ANISOTROPY is 1, 2, 4, 8 or 16, not %llu", asked);
            CONSOLE
        }
    }
});

guest_hook!(T_Dvar_RegisterInt, |ctx, memory| {
    let name = ctx.r3.as_u32();
    // Its default, between its minimum and maximum (r4, r5, r6).
    if name != 0 && memory.holds(name, c"r_texFilterAnisoMin") {
        let (asked, least, most) = (*ASKED, ctx.r5.as_u32(), ctx.r6.as_u32());
        if asked != CONSOLE && (least..=most).contains(&asked) && most <= MOST {
            ctx.r4.u64 = asked.into();
        }
    }
    T_Dvar_RegisterInt.original(ctx, memory);
});
