#pragma once
// Minimal stand-in for the generated ppc_recomp_shared.h, so asset-free tests
// can compile the runtime's kernel sources without any recompiled game code.
// It declares only the types the kernel headers name; nothing here executes
// guest code, and no game data is involved.
#include <cstdint>

struct PPCRegister
{
    union { int64_t s64; uint64_t u64; int32_t s32; uint32_t u32; };
    PPCRegister() : u64(0) {}
};

struct PPCFpscr { void loadFromHost() {} };

struct PPCContext
{
    PPCFpscr fpscr;
    PPCRegister r0, r1, r2, r3, r4, r5, r6, r7, r8, r9, r10, r11, r12, r13, r14, r15, r16,
        r17, r18, r19, r20, r21, r22, r23, r24, r25, r26, r27, r28, r29, r30, r31;
};

inline constexpr uint32_t PPC_CODE_BASE = 0x82000000;
inline constexpr uint32_t PPC_CODE_SIZE = 0x00100000;

using PPCFunc = void(PPCContext& ctx, uint8_t* base);

#define PPC_FUNC(x) void x(PPCContext& __restrict ctx, uint8_t* base)
