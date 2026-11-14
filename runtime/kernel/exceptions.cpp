// RtlRaiseException.
//
//   void RtlRaiseException(EXCEPTION_RECORD* record);
//
// EXCEPTION_RECORD (big-endian words): code +0, flags +4, next record +8,
// address +12, parameter count +16, parameters +20 onward.
//
// MW3 raises 0x406D1388 with four parameters {0x1000, name, thread id, 0} through
// sub_82458038's callers (sub_8245xxxx CRT wrappers and its thread start-up):
// the Windows "name this thread" convention. A debugger would consume it; here
// the calling guest thread is a host thread, so it is named for logs and
// debuggers. Any other code has no structured-exception handler to reach in
// this runtime: it is logged and the call returns, as for an exception a
// handler chose to ignore.
#include <ppc_recomp_shared.h>

#include "../guest.h"
#include "../log.h"
#include "kernel.h"

#include <cstdio>
#include <string>

#if defined(__linux__)
#include <pthread.h>
#endif

namespace
{
    constexpr uint32_t kSetThreadNameCode = 0x406D1388;
    constexpr uint32_t kSetThreadNameType = 0x1000;
}

PPC_FUNC(__imp__RtlRaiseException)
{
    const auto* record = GuestPtr<const be32>(ctx.r3.u32);
    if (!record) return;

    const uint32_t code = record[0];
    const uint32_t params = record[4];

    if (code == kSetThreadNameCode && params >= 3 && uint32_t(record[5]) == kSetThreadNameType)
    {
        const uint32_t namePtr = record[6];
        const uint32_t threadId = record[7];
        std::string name;
        if (namePtr)
        {
            const auto* text = GuestPtr<const uint8_t>(namePtr);
            for (uint32_t i = 0; i < 64 && text[i]; i++)
                name += (text[i] >= 32 && text[i] < 127) ? char(text[i]) : '?';
        }
        // A thread id of -1 names the caller, which is the only case a debugger
        // convention needs; another id would need a thread-id table.
        if (threadId == 0xFFFFFFFFu || threadId == 0)
        {
#if defined(__linux__)
            pthread_setname_np(pthread_self(), name.substr(0, 15).c_str());   // 15 chars + NUL
#endif
            LOGI("thread named \"%s\"", name.c_str());
        }
        else
            LOGI("thread %u named \"%s\" (not applied)", threadId, name.c_str());
        return;
    }

    LOGW("RtlRaiseException: code %08X, flags %08X, %u parameters, address %08X: no handler, returning",
         code, uint32_t(record[1]), params, uint32_t(record[3]));
}
