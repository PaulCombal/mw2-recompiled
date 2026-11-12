// Xbox 360 CRT formatters: sprintf, _snprintf, _vsnprintf, vswprintf.
//
// Calling convention (read off the MW3 wrappers, e.g. sub_82459710): the
// parameter area is 8-byte slots starting at r1+0x10, slot N holding argument
// N (r3 = slot 0). A variadic function spills r5..r10 there and its va_list is
// a pointer to the first variadic slot; slots past r10 follow on the stack
// (slot 8 at r1+0x50). A 32-bit value is the low word of its big-endian slot.
// Floating-point arguments are not supported (see guest_format.h).
#include <ppc_recomp_shared.h>

#include "guest_format.h"
#include "../guest.h"
#include "kernel.h"
#include "../log.h"

namespace
{
    // Variadic arguments passed directly: registers first, then the stack.
    struct DirectArgs
    {
        PPCContext& ctx;
        uint32_t slot;   // next argument slot; r3 is slot 0
        uint64_t next()
        {
            const uint32_t n = slot++;
            if (n < 8)
            {
                const uint64_t regs[] = {ctx.r3.u64, ctx.r4.u64, ctx.r5.u64, ctx.r6.u64,
                                         ctx.r7.u64, ctx.r8.u64, ctx.r9.u64, ctx.r10.u64};
                return regs[n];
            }
            return uint64_t(*GuestPtr<be64>(ctx.r1.u32 + 0x10 + 8 * n));
        }
    };

    // A va_list: a guest pointer to consecutive 8-byte slots.
    struct VaList
    {
        uint32_t ptr;
        uint64_t next()
        {
            const uint64_t v = *GuestPtr<be64>(ptr);
            ptr += 8;
            return v;
        }
    };

    // Writes the text as narrow or wide into [dest, dest+capacity) and returns
    // the C-runtime result. `capacity` is the character count the caller allows.
    // Narrow output keeps the low byte of each code point.
    uint32_t Emit(const std::u32string& text, uint32_t dest, bool wide, uint32_t capacity,
                  bool counted)
    {
        const uint32_t len = uint32_t(text.size());
        const uint32_t n = len < capacity ? len : capacity;
        for (uint32_t i = 0; i < n; i++)
        {
            if (wide) *GuestPtr<be16>(dest + 2 * i) = uint16_t(text[i]);
            else *GuestPtr<uint8_t>(dest + i) = uint8_t(text[i]);
        }
        // Terminator when it fits. _snprintf truncation returns -1 and leaves
        // no terminator when the text fills the buffer exactly.
        if (n < capacity)
        {
            if (wide) *GuestPtr<be16>(dest + 2 * n) = 0;
            else *GuestPtr<uint8_t>(dest + n) = 0;
        }
        if (counted && len > capacity) return uint32_t(-1);
        return len;
    }

    template<class Args>
    void Run(PPCContext& ctx, const char* name, uint32_t dest, uint32_t fmt, bool wide,
             uint32_t capacity, bool counted, Args args)
    {
        if (!dest || !fmt) { ctx.r3.s64 = -1; return; }
        auto r = kernel::fmt::Format(guest::Base(), fmt, wide, args);
        if (r.unsupported)
            LOGW("%s: unsupported conversion (floating point?) in \"%s\"", name,
                 (wide ? GuestWideToUtf8(fmt) : GuestAnsi(fmt)).c_str());
        ctx.r3.s64 = int32_t(Emit(r.text, dest, wide, capacity, counted));
    }
}

// int sprintf(char* buf, const char* fmt, ...)
PPC_FUNC(__imp__sprintf)
{
    Run(ctx, "sprintf", ctx.r3.u32, ctx.r4.u32, false, 0x7FFFFFFF, false, DirectArgs{ctx, 2});
}

// int _snprintf(char* buf, size_t count, const char* fmt, ...)
PPC_FUNC(__imp___snprintf)
{
    Run(ctx, "_snprintf", ctx.r3.u32, ctx.r5.u32, false, ctx.r4.u32, true, DirectArgs{ctx, 3});
}

// int _vsnprintf(char* buf, size_t count, const char* fmt, va_list args)
PPC_FUNC(__imp___vsnprintf)
{
    Run(ctx, "_vsnprintf", ctx.r3.u32, ctx.r5.u32, false, ctx.r4.u32, true, VaList{ctx.r6.u32});
}

// int vswprintf(wchar_t* buf, const wchar_t* fmt, va_list args)  (no count)
PPC_FUNC(__imp__vswprintf)
{
    Run(ctx, "vswprintf", ctx.r3.u32, ctx.r4.u32, true, 0x7FFFFFFF, false, VaList{ctx.r5.u32});
}
