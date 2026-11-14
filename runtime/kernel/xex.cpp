// Xbox 360 XEX optional-header lookup.
#include <ppc_recomp_shared.h>

#include "xex_header.h"
#include "../guest.h"
#include "kernel.h"

PPC_FUNC(__imp__RtlImageXexHeaderField)
{
    ctx.r3.u64 = kernel::xex::ImageHeaderField(
        guest::Base(), ctx.r3.u32, ctx.r4.u32);
}

// XamGetExecutionId(PXEX_EXECUTION_ID* out): hands back a pointer to the
// execution-info block (key 0x40006) inside the executable's XEX header --
// title id, version, media id. Nonzero (failure) when there is no header.
PPC_FUNC(__imp__XamGetExecutionId)
{
    const uint32_t header = kernel::ExecutableXexHeader();
    const uint32_t info = kernel::xex::ImageHeaderField(guest::Base(), header, 0x40006);
    if (!info || !ctx.r3.u32) { ctx.r3.u64 = 0xC0000001u; return; }
    *GuestPtr<be32>(ctx.r3.u32) = info;
    ctx.r3.u64 = 0;
}

// XGetGameRegion: the region mask in the XEX security info (offset 0x10 of the
// header points at it; the field is at +376). 0x0000FD00 for this title; with
// no header, report all-regions-free (0xFFFFFFFF).
PPC_FUNC(__imp__XGetGameRegion)
{
    const uint32_t header = kernel::ExecutableXexHeader();
    uint32_t region = 0xFFFFFFFFu;
    if (header)
    {
        const uint32_t sec = *GuestPtr<be32>(header + 0x10);
        if (sec) region = *GuestPtr<be32>(header + sec + 376);
    }
    ctx.r3.u64 = region;
}
