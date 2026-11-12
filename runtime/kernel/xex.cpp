// Xbox 360 XEX optional-header lookup.
#include <ppc_recomp_shared.h>

#include "xex_header.h"
#include "../guest.h"

PPC_FUNC(__imp__RtlImageXexHeaderField)
{
    ctx.r3.u64 = kernel::xex::ImageHeaderField(
        guest::Base(), ctx.r3.u32, ctx.r4.u32);
}
