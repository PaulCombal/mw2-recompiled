// RtlUnicodeStringToAnsiString / RtlFreeAnsiString.
//
//   NTSTATUS RtlUnicodeStringToAnsiString(ANSI_STRING* dest,
//                                         const UNICODE_STRING* source,
//                                         BOOLEAN allocateDestination);
//   void RtlFreeAnsiString(ANSI_STRING* string);
//
// Both string structs are { USHORT Length; USHORT MaximumLength; PVOID Buffer }
// with Length in bytes, excluding the terminator. Text is converted one UTF-16
// unit to one byte; units outside ASCII become '?'.
#include <ppc_recomp_shared.h>

#include "../guest.h"
#include "kernel.h"

namespace
{
    constexpr uint32_t kStatusBufferOverflow = 0x80000005;

    struct XString { be16 length; be16 maximumLength; be32 buffer; };
}

PPC_FUNC(__imp__RtlUnicodeStringToAnsiString)
{
    auto* dest = GuestPtr<XString>(ctx.r3.u32);
    const auto* source = GuestPtr<const XString>(ctx.r4.u32);
    const bool allocate = (ctx.r5.u32 & 0xFF) != 0;
    if (!dest || !source) { ctx.r3.u32 = X_STATUS_INVALID_PARAMETER; return; }

    const uint32_t chars = uint16_t(source->length) / 2;
    if (chars + 1 > 0xFFFF) { ctx.r3.u32 = X_STATUS_INVALID_PARAMETER; return; }

    uint32_t buffer;
    if (allocate)
    {
        buffer = kernel::AllocateGuest(chars + 1, 1);
        if (!buffer) { ctx.r3.u32 = X_STATUS_NO_MEMORY; return; }
        dest->maximumLength = uint16_t(chars + 1);
    }
    else
    {
        buffer = dest->buffer;
        if (uint16_t(dest->maximumLength) < chars || (!buffer && chars))
        {
            ctx.r3.u32 = kStatusBufferOverflow;
            return;
        }
    }

    const auto* in = GuestPtr<const be16>(source->buffer);
    auto* out = GuestPtr<uint8_t>(buffer);
    for (uint32_t i = 0; i < chars; i++)
    {
        const uint16_t c = in[i];
        out[i] = c < 0x80 ? uint8_t(c) : uint8_t('?');
    }
    if (out && uint16_t(dest->maximumLength) > chars) out[chars] = 0;

    dest->length = uint16_t(chars);
    dest->buffer = buffer;
    ctx.r3.u32 = X_STATUS_SUCCESS;
}

PPC_FUNC(__imp__RtlFreeAnsiString)
{
    auto* string = GuestPtr<XString>(ctx.r3.u32);
    if (!string) return;
    if (string->buffer) kernel::FreeGuest(string->buffer);
    string->length = 0;
    string->maximumLength = 0;
    string->buffer = 0;
}
