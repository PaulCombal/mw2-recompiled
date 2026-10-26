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
#include <ppc_recomp_shared.h>
#include "title.h"
#include "env.h"
#include "log.h"

#include <cstring>

#ifdef T_Dvar_RegisterInt

namespace
{
    constexpr uint32_t kConsole = 1, kMost = 16;

    uint32_t Asked()
    {
        static const uint32_t asked = [] {
            const uint64_t n = env::Number("MW2_ANISOTROPY", kConsole);
            if (n != 1 && n != 2 && n != 4 && n != 8 && n != 16)
            {
                LOGW("anisotropy: MW2_ANISOTROPY is 1, 2, 4, 8 or 16, not %llu", static_cast<unsigned long long>(n));
                return kConsole;
            }
            if (n != kConsole) LOGI("anisotropy: textures are filtered at %llux at least", static_cast<unsigned long long>(n));
            return uint32_t(n);
        }();
        return asked;
    }
}

GUEST_HOOK(T_Dvar_RegisterInt)
{
    const char* name = reinterpret_cast<const char*>(base + ctx.r3.u32);
    // Its default, between its minimum and maximum (r4, r5, r6).
    if (ctx.r3.u32 && !std::strcmp(name, "r_texFilterAnisoMin") && Asked() != kConsole &&
        ctx.r5.u32 <= Asked() && Asked() <= ctx.r6.u32 && ctx.r6.u32 <= kMost)
        ctx.r4.u64 = Asked();
    GUEST_ORIG(T_Dvar_RegisterInt)(ctx, base);
}

#endif
