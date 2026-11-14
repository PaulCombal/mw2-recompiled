#pragma once
// printf-style formatting for the Xbox 360 CRT imports (sprintf, _snprintf,
// _vsnprintf, vswprintf). The format string, %s arguments and the output are
// guest memory (big-endian for wide text); the arguments come from an
// ArgSource, which yields one 8-byte parameter slot at a time.
//
// Supported: flags - + space 0 #, width and precision (number or *), length
// h l ll I64 w, conversions d i u x X o c s S p %. Floating-point conversions
// (e f g a) are NOT supported: they live in FP registers on PPC and no
// reachable MW3 call site uses them (docs/MW3-STARTUP-IMPORT-PLAN.md). They
// set `unsupported` and are skipped rather than guessed.
//
// Microsoft CRT conventions: in the narrow functions %s is char* and %S is
// wchar_t*; in the wide function it is the other way round.

#include "../guest.h"

#include <cstdint>
#include <string>

namespace kernel::fmt
{
    struct Result
    {
        std::u32string text;   // code points; the caller narrows or widens
        bool unsupported = false;
    };

    // Yields the next parameter slot. A 32-bit value is the low word of the
    // big-endian 8-byte slot.
    template<class ArgSource>
    Result Format(const uint8_t* base, uint32_t fmtAddr, bool wideFormat, ArgSource& args)
    {
        constexpr uint32_t kMaxScan = 1u << 16;
        Result out;
        auto fchar = [&](uint32_t i) -> char32_t {
            if (i >= kMaxScan) return 0;
            if (wideFormat) return uint16_t(reinterpret_cast<const be16*>(base + fmtAddr)[i]);
            return base[fmtAddr + i];
        };
        auto readStr = [&](uint32_t addr, bool wide, int64_t limit, std::u32string& s) {
            if (!addr) { s = U"(null)"; return; }
            for (uint32_t i = 0; i < kMaxScan; i++)
            {
                if (limit >= 0 && int64_t(i) >= limit) break;
                char32_t c = wide ? char32_t(uint16_t(reinterpret_cast<const be16*>(base + addr)[i]))
                                  : char32_t(base[addr + i]);
                if (!c) break;
                s += c;
            }
        };

        for (uint32_t i = 0; fchar(i);)
        {
            char32_t c = fchar(i++);
            if (c != U'%') { out.text += c; continue; }

            bool left = false, plus = false, space = false, zero = false, alt = false;
            for (;; i++)
            {
                char32_t f = fchar(i);
                if (f == U'-') left = true; else if (f == U'+') plus = true;
                else if (f == U' ') space = true; else if (f == U'0') zero = true;
                else if (f == U'#') alt = true; else break;
            }
            int64_t width = 0, prec = -1;
            if (fchar(i) == U'*')
            {
                i++;
                int32_t w = int32_t(args.next());
                if (w < 0) { left = true; w = -w; }
                width = w;
            }
            else while (fchar(i) >= U'0' && fchar(i) <= U'9') width = width * 10 + (fchar(i++) - U'0');
            if (fchar(i) == U'.')
            {
                i++; prec = 0;
                if (fchar(i) == U'*') { i++; int32_t p = int32_t(args.next()); prec = p < 0 ? -1 : p; }
                else while (fchar(i) >= U'0' && fchar(i) <= U'9') prec = prec * 10 + (fchar(i++) - U'0');
            }
            if (width > 4096) width = 4096;

            enum { kDefault, kShort, kLong, kLongLong } len = kDefault;
            if (fchar(i) == U'h') { len = kShort; i++; }
            else if (fchar(i) == U'l') { i++; len = kLong; if (fchar(i) == U'l') { i++; len = kLongLong; } }
            else if (fchar(i) == U'w') { len = kLong; i++; }
            else if (fchar(i) == U'I' && fchar(i + 1) == U'6' && fchar(i + 2) == U'4') { len = kLongLong; i += 3; }

            char32_t conv = fchar(i);
            if (!conv) break;
            i++;

            std::u32string body;
            if (conv == U'%') { out.text += U'%'; continue; }
            else if (conv == U'c')
            {
                body += char32_t(args.next() & (wideFormat || len == kLong ? 0xFFFF : 0xFF));
            }
            else if (conv == U's' || conv == U'S')
            {
                // %s is the format's own width; %S the other one; l/w force wide, h narrow.
                bool wide = (conv == U's') == wideFormat;
                if (len == kLong) wide = true; else if (len == kShort) wide = false;
                readStr(uint32_t(args.next()), wide, prec, body);
            }
            else if (conv == U'd' || conv == U'i' || conv == U'u' || conv == U'x' ||
                     conv == U'X' || conv == U'o' || conv == U'p')
            {
                uint64_t raw = args.next();
                bool sign = conv == U'd' || conv == U'i';
                if (conv == U'p') { conv = U'X'; zero = true; width = 8; len = kDefault; }
                uint64_t mag; bool neg = false;
                if (len == kLongLong) mag = raw;
                else if (len == kShort) mag = sign ? uint64_t(int64_t(int16_t(raw))) : uint16_t(raw);
                else mag = sign ? uint64_t(int64_t(int32_t(raw))) : uint32_t(raw);
                if (sign && int64_t(mag) < 0) { neg = true; mag = 0 - mag; }
                unsigned radix = (conv == U'x' || conv == U'X') ? 16 : conv == U'o' ? 8 : 10;
                const char* digits = conv == U'X' ? "0123456789ABCDEF" : "0123456789abcdef";
                std::u32string num;
                do { num.insert(num.begin(), char32_t(digits[mag % radix])); mag /= radix; } while (mag);
                if (prec >= 0) { while (int64_t(num.size()) < prec) num.insert(num.begin(), U'0'); }
                std::u32string prefix;
                if (neg) prefix = U"-"; else if (sign && plus) prefix = U"+"; else if (sign && space) prefix = U" ";
                if (alt && radix == 16 && num != U"0") prefix += conv == U'X' ? U"0X" : U"0x";
                if (alt && radix == 8 && num[0] != U'0') num.insert(num.begin(), U'0');
                if (zero && !left && prec < 0)
                    while (int64_t(prefix.size() + num.size()) < width) num.insert(num.begin(), U'0');
                body = prefix + num;
            }
            else { out.unsupported = true; body = std::u32string(U"%") + conv; }

            int64_t pad = width - int64_t(body.size());
            if (pad > 0 && !left) out.text.append(size_t(pad), U' ');
            out.text += body;
            if (pad > 0 && left) out.text.append(size_t(pad), U' ');
        }
        return out;
    }
}
