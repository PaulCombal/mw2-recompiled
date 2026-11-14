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

struct PPCContext
{
    PPCRegister r0, r1, r2, r3, r4, r5, r6, r7, r8, r9, r10, r11, r12, r13;
};

using PPCFunc = void(PPCContext& ctx, uint8_t* base);
