// The clocks, delays and small queries the title makes of the kernel.
#include <ppc_recomp_shared.h>
#include "../pacing_trace.h"
#include "kernel.h"
#include "../guest.h"
#include "../log.h"
#include "../crash.h"
#include "../install/install.h"

#include <algorithm>
#include <cctype>
#include <chrono>
#include <cstdlib>
#include <cstring>
#include <string>
#include <thread>
#include <limits>

uint64_t kernel::SystemTime100ns()
{
    using namespace std::chrono;
    auto now = system_clock::now().time_since_epoch();
    return uint64_t(duration_cast<nanoseconds>(now).count() / 100) + 116444736000000000ull;
}

PPC_FUNC(__imp__KeGetCurrentProcessType) { ctx.r3.u64 = 1; }   // title

PPC_FUNC(__imp__XexCheckExecutablePrivilege) { ctx.r3.u64 = 0; }

PPC_FUNC(__imp__HalReturnToFirmware)
{
    LOGI("HalReturnToFirmware(%u) -- guest asked to shut down", ctx.r3.u32);
    kernel::ReportUnimplemented();
    std::exit(0);
}

// The guest timebase, which every mftb in the recompiled code reads. It runs at
// the console's 50 MHz, not at the host's cycle counter: the title only
// sometimes asks KeQueryPerformanceFrequency, and MW2's server thread times its
// frames with a constant 1/50000 ms per tick -- on a faster counter the game ran
// at the frame rate instead of its own clock.
namespace
{
    constexpr uint64_t kTimebaseHz = 50000000;
    const auto g_timebaseStart = std::chrono::steady_clock::now();
}

uint64_t PPC_QueryTimebase()
{
    const auto elapsed = std::chrono::steady_clock::now() - g_timebaseStart;
    return uint64_t(std::chrono::duration_cast<std::chrono::nanoseconds>(elapsed).count()) /
           (1000000000ull / kTimebaseHz);
}

PPC_FUNC(__imp__KeQueryPerformanceFrequency) { ctx.r3.u64 = kTimebaseHz; }

PPC_FUNC(__imp__KeQuerySystemTime)
{
    auto* out = GuestPtr<be64>(ctx.r3.u32);
    if (out) *out = kernel::SystemTime100ns();
}

// TIME_FIELDS is eight big-endian SHORTs. NT LARGE_INTEGER time is
// measured in 100ns units from 1601-01-01 UTC.
namespace
{
    struct XTimeFields
    {
        be16 year;
        be16 month;
        be16 day;
        be16 hour;
        be16 minute;
        be16 second;
        be16 milliseconds;
        be16 weekday;
    };

    constexpr uint64_t kNtTicksPerMillisecond = 10000;
    constexpr uint64_t kNtTicksPerSecond = 10000000;
    constexpr uint64_t kNtTicksPerDay = 864000000000;
    constexpr std::chrono::sys_days kNtEpoch =
        std::chrono::sys_days{ std::chrono::year{1601} / 1 / 1 };
}

PPC_FUNC(__imp__RtlTimeToTimeFields)
{
    const auto* time = GuestPtr<const be64>(ctx.r3.u32);
    auto* fields = GuestPtr<XTimeFields>(ctx.r4.u32);
    if (!time || !fields) return;

    const uint64_t ticks = uint64_t(*time);
    const uint64_t dayCount = ticks / kNtTicksPerDay;
    uint64_t remainder = ticks % kNtTicksPerDay;
    const auto date = kNtEpoch + std::chrono::days{ int64_t(dayCount) };
    const std::chrono::year_month_day ymd{ date };

    fields->year = int16_t(int(ymd.year()));
    fields->month = uint16_t(unsigned(ymd.month()));
    fields->day = uint16_t(unsigned(ymd.day()));
    fields->weekday = uint16_t(std::chrono::weekday{ date }.c_encoding());
    fields->hour = uint16_t(remainder / (3600 * kNtTicksPerSecond));
    remainder %= 3600 * kNtTicksPerSecond;
    fields->minute = uint16_t(remainder / (60 * kNtTicksPerSecond));
    remainder %= 60 * kNtTicksPerSecond;
    fields->second = uint16_t(remainder / kNtTicksPerSecond);
    remainder %= kNtTicksPerSecond;
    fields->milliseconds = uint16_t(remainder / kNtTicksPerMillisecond);
}

PPC_FUNC(__imp__RtlTimeFieldsToTime)
{
    const auto* fields = GuestPtr<const XTimeFields>(ctx.r3.u32);
    auto* time = GuestPtr<be64>(ctx.r4.u32);
    if (!fields || !time) { ctx.r3.u64 = 0; return; }

    const int yearValue = int16_t(fields->year);
    const unsigned monthValue = uint16_t(fields->month);
    const unsigned dayValue = uint16_t(fields->day);
    const unsigned hourValue = uint16_t(fields->hour);
    const unsigned minuteValue = uint16_t(fields->minute);
    const unsigned secondValue = uint16_t(fields->second);
    const unsigned millisecondValue = uint16_t(fields->milliseconds);

    const std::chrono::year_month_day date{
        std::chrono::year{ yearValue },
        std::chrono::month{ monthValue },
        std::chrono::day{ dayValue }
    };
    if (yearValue < 1601 || !date.ok() || hourValue > 23 ||
        minuteValue > 59 || secondValue > 59 || millisecondValue > 999)
    {
        ctx.r3.u64 = 0;
        return;
    }

    const int64_t dayCount = (std::chrono::sys_days{ date } - kNtEpoch).count();
    if (dayCount < 0) { ctx.r3.u64 = 0; return; }

    const uint64_t ticks =
        uint64_t(dayCount) * kNtTicksPerDay +
        uint64_t(hourValue) * 3600 * kNtTicksPerSecond +
        uint64_t(minuteValue) * 60 * kNtTicksPerSecond +
        uint64_t(secondValue) * kNtTicksPerSecond +
        uint64_t(millisecondValue) * kNtTicksPerMillisecond;
    if (ticks > uint64_t(std::numeric_limits<int64_t>::max()))
    {
        ctx.r3.u64 = 0;
        return;
    }

    *time = ticks;
    ctx.r3.u64 = 1;
}

PPC_FUNC(__imp__KeDelayExecutionThread)
{
    // (processorMode, alertable, intervalPtr) -- 100ns, negative = relative
    if (ctx.r4.u32 && kernel::DeliverUserApcs(ctx)) { ctx.r3.u64 = X_STATUS_USER_APC; return; }
    auto* interval = GuestPtr<be64>(ctx.r5.u32);
    if (interval)
    {
        const int64_t v = int64_t(uint64_t(*interval));
        // The most negative value is "infinite", and negating it is undefined, so it
        // is handled before the arithmetic rather than after.
        if (v == INT64_MIN)
        {
            LOGW("KeDelayExecutionThread: an infinite wait; the caller expects an APC or an alert");
            std::this_thread::sleep_for(std::chrono::milliseconds(1));
        }
        else if (v < 0)
        {
            const int64_t ns = -v * 100;
            if (ns > 1000000000ll)
                LOGW("KeDelayExecutionThread: sleeping %.1f seconds", double(ns) / 1e9);
            const bool traced = pacing::On();
            if (traced) { pacing::Note(pacing::kSleep, uint32_t(ctx.lr)); pacing::NoteCallers(ctx.r1.u32); }
            std::this_thread::sleep_for(std::chrono::nanoseconds(ns));
            if (traced) pacing::Note(pacing::kKernelWaitEnd);
        }
        else
        {
            std::this_thread::yield();
        }
    }
    ctx.r3.u64 = X_STATUS_SUCCESS;
}

// The campaign and the multiplayer are two executables, and the title goes
// from one to the other by asking the loader for it. What it wants the other
// to know -- which menu to open, who was signed in -- it leaves as launch
// data first, and reads back at its own start (runtime/install/).
// (buffer, size)
PPC_FUNC(__imp__XamLoaderGetLaunchData)
{
    const auto& data = install::LaunchData();
    if (data.empty()) { ctx.r3.u64 = X_ERROR_NOT_FOUND; return; }
    std::memcpy(GuestPtr<uint8_t>(ctx.r3.u32), data.data(), std::min<size_t>(data.size(), ctx.r4.u32));
    ctx.r3.u64 = X_ERROR_SUCCESS;
}
// (data, size)
PPC_FUNC(__imp__XamLoaderSetLaunchData)
{
    const auto* data = GuestPtr<const uint8_t>(ctx.r3.u32);
    install::LaunchData().assign(data, data + ctx.r4.u32);
    // MW3's fatal-error path (sub_8232E638) formats "e%s" with its message into
    // this buffer and then relaunches default.xex, so a startup failure surfaces
    // here as text. Show the printable head of it.
    // The buffer opens with a small header (the message sits at +4), so start at
    // the first printable byte within its head.
    std::string text;
    uint32_t at = 0;
    while (at < 16 && at < ctx.r4.u32 && !(data[at] >= 32 && data[at] < 127)) at++;
    for (uint32_t i = at; i < ctx.r4.u32 && i < at + 240 && data[i]; i++)
        text += (data[i] >= 32 && data[i] < 127) ? char(data[i]) : '?';
    if (!text.empty()) LOGW("XamLoaderSetLaunchData: %u bytes, text \"%s\"", ctx.r4.u32, text.c_str());
    ctx.r3.u64 = X_ERROR_SUCCESS;
}

namespace
{
    // Neither call returns on the console: the title is gone when it would.
    [[noreturn]] void EndTitle(const char* why)
    {
        crash::RequestExit(why);
        for (;;) std::this_thread::sleep_for(std::chrono::seconds(1));
    }
}
// (path, flags). No path is the way back to the dashboard.
PPC_FUNC(__imp__XamLoaderLaunchTitle)
{
    if (const char* path = GuestPtr<const char>(ctx.r3.u32))
    {
        std::string name = path;
        if (const size_t slash = name.find_last_of("\\/:"); slash != std::string::npos) name.erase(0, slash + 1);
        for (char& c : name) c = char(std::tolower(uint8_t(c)));
        LOGI("XamLoaderLaunchTitle: %s (flags %X)", path, ctx.r4.u32);
        install::SetNextTitle(name);
        EndTitle("the title started its other executable");
    }
    EndTitle("the title left for the dashboard");
}
PPC_FUNC(__imp__XamLoaderTerminateTitle) { EndTitle("the title ended itself"); }

// The lowest version with both XSessionMigrateHost and XSessionModifySkill, which
// the multiplayer does not send on an older one. Newer ones add nothing the title
// can use: it looks for one more entry point in xam.xex at 0x20211000.
PPC_FUNC(__imp__XamGetSystemVersion)    { ctx.r3.u64 = 0x200ED800; }

// No system module is there to load or look into. The title's own wrappers
// take "not found" as the answer to fall back from; a success that wrote no
// handle sent them into whatever the stack held.
namespace
{
    constexpr uint32_t X_STATUS_DLL_NOT_FOUND = 0xC0000135;
    constexpr uint32_t X_STATUS_ENTRYPOINT_NOT_FOUND = 0xC0000139;
}
// (name, handleOut)
PPC_FUNC(__imp__XexGetModuleHandle)
{
    if (auto* handle = GuestPtr<be32>(ctx.r4.u32)) *handle = 0;
    ctx.r3.u64 = X_STATUS_DLL_NOT_FOUND;
}
// (module, ordinal, addressOut)
PPC_FUNC(__imp__XexGetProcedureAddress)
{
    if (auto* address = GuestPtr<be32>(ctx.r5.u32)) *address = 0;
    ctx.r3.u64 = X_STATUS_ENTRYPOINT_NOT_FOUND;
}
// (name, flags, minimumVersion, handleOut)
PPC_FUNC(__imp__XexLoadImage)
{
    if (auto* handle = GuestPtr<be32>(ctx.r6.u32)) *handle = 0;
    ctx.r3.u64 = X_STATUS_DLL_NOT_FOUND;
}
PPC_FUNC(__imp__XexUnloadImage)         { ctx.r3.u64 = X_STATUS_SUCCESS; }
PPC_FUNC(__imp__XGetAVPack)             { ctx.r3.u64 = 0x010000; }   // HDMI
PPC_FUNC(__imp__XGetLanguage)           { ctx.r3.u64 = 1; }          // English
PPC_FUNC(__imp__KeLockL2)               { ctx.r3.u64 = 0; }
PPC_FUNC(__imp__KeUnlockL2)             { }
PPC_FUNC(__imp__KiApcNormalRoutineNop)  { ctx.r3.u64 = 0; }

PPC_FUNC(__imp__DbgPrint)
{
    // Guest varargs are awkward and the format string alone is the useful part.
    std::string fmt = GuestAnsi(ctx.r3.u32);
    if (!fmt.empty() && fmt.back() == '\n') fmt.pop_back();
    LOGK("DbgPrint: %s", fmt.c_str());
}

// NTSTATUS -> Win32 error. Called on every failed operation, so only the codes
// the runtime actually returns matter.
PPC_FUNC(__imp__RtlNtStatusToDosError)
{
    uint32_t status = ctx.r3.u32;
    switch (status)
    {
    case X_STATUS_SUCCESS:                 ctx.r3.u64 = 0;    return;  // ERROR_SUCCESS
    case X_STATUS_OBJECT_NAME_NOT_FOUND:
    case X_STATUS_NO_SUCH_FILE:            ctx.r3.u64 = 2;    return;  // ERROR_FILE_NOT_FOUND
    case X_STATUS_END_OF_FILE:             ctx.r3.u64 = 38;   return;  // ERROR_HANDLE_EOF
    case X_STATUS_INVALID_PARAMETER:       ctx.r3.u64 = 87;   return;  // ERROR_INVALID_PARAMETER
    case X_STATUS_NO_MEMORY:               ctx.r3.u64 = 8;    return;  // ERROR_NOT_ENOUGH_MEMORY
    case X_STATUS_TIMEOUT:                 ctx.r3.u64 = 1460; return;  // ERROR_TIMEOUT
    default:                               ctx.r3.u64 = 317;  return;  // ERROR_MR_MID_NOT_FOUND
    }
}

// Without this every OBJECT_ATTRIBUTES the title builds carries an empty name,
// so no file can ever be opened.
PPC_FUNC(__imp__RtlInitAnsiString)
{
    struct XAnsiString { be16 length; be16 maximumLength; be32 pointer; };
    auto* dest = GuestPtr<XAnsiString>(ctx.r3.u32);
    if (!dest) return;

    uint32_t source = ctx.r4.u32;
    if (!source)
    {
        dest->length = 0;
        dest->maximumLength = 0;
        dest->pointer = 0;
        return;
    }

    size_t length = std::strlen(reinterpret_cast<const char*>(guest::Base() + source));
    dest->length = uint16_t(length);
    dest->maximumLength = uint16_t(length + 1);
    dest->pointer = source;
}

// (UNICODE_STRING*, source). The source buffer remains owned by the
// caller; this routine only records its byte length and pointer.
PPC_FUNC(__imp__RtlInitUnicodeString)
{
    struct XUnicodeString { be16 length; be16 maximumLength; be32 pointer; };
    auto* dest = GuestPtr<XUnicodeString>(ctx.r3.u32);
    if (!dest) return;

    const uint32_t source = ctx.r4.u32;
    if (!source)
    {
        dest->length = 0;
        dest->maximumLength = 0;
        dest->pointer = 0;
        return;
    }

    const auto* text = GuestPtr<const be16>(source);
    if (!text)
    {
        dest->length = 0;
        dest->maximumLength = 0;
        dest->pointer = 0;
        return;
    }

    // UNICODE_STRING lengths are byte counts and exclude the terminator.
    // Length fields are USHORTs, so cap the scan before multiplication wraps.
    uint32_t characters = 0;
    while (characters < 0x7FFE && uint16_t(text[characters]) != 0) ++characters;
    dest->length = uint16_t(characters * 2);
    dest->maximumLength = uint16_t((characters + 1) * 2);
    dest->pointer = source;
}

// The kernel's code page is Latin-1 as far as the title's text goes: each byte
// is one UTF-16 unit. Both take byte counts and write back how many bytes they
// produced. The title builds Live storage file names with the first.
//
// (unicodeOut, maxBytesOut, bytesOut, multiByte, bytesIn)
PPC_FUNC(__imp__RtlMultiByteToUnicodeN)
{
    auto* out = GuestPtr<be16>(ctx.r3.u32);
    const auto* in = GuestPtr<const uint8_t>(ctx.r6.u32);
    const uint32_t count = (out && in) ? std::min(ctx.r4.u32 / 2, ctx.r7.u32) : 0;
    for (uint32_t i = 0; i < count; i++) out[i] = in[i];
    if (auto* written = GuestPtr<be32>(ctx.r5.u32)) *written = count * 2;
    ctx.r3.u64 = X_STATUS_SUCCESS;
}

// (multiByteOut, maxBytesOut, bytesOut, unicode, bytesIn)
PPC_FUNC(__imp__RtlUnicodeToMultiByteN)
{
    auto* out = GuestPtr<uint8_t>(ctx.r3.u32);
    const auto* in = GuestPtr<const be16>(ctx.r6.u32);
    const uint32_t count = (out && in) ? std::min(ctx.r4.u32, ctx.r7.u32 / 2) : 0;
    for (uint32_t i = 0; i < count; i++)
    {
        const uint16_t c = in[i];
        out[i] = c < 0x100 ? uint8_t(c) : uint8_t('?');
    }
    if (auto* written = GuestPtr<be32>(ctx.r5.u32)) *written = count;
    ctx.r3.u64 = X_STATUS_SUCCESS;
}

PPC_FUNC(__imp__RtlFillMemoryUlong)
{
    uint32_t length = ctx.r4.u32 & ~3u;
    auto* dest = GuestPtr<be32>(ctx.r3.u32);
    if (!dest) return;
    for (uint32_t i = 0; i < length / 4; i++) dest[i] = ctx.r5.u32;
}

PPC_FUNC(__imp__RtlCompareMemoryUlong)
{
    uint32_t length = ctx.r4.u32 & ~3u;
    auto* src = GuestPtr<be32>(ctx.r3.u32);
    uint32_t matched = 0;
    if (src)
        while (matched < length && uint32_t(src[matched / 4]) == ctx.r5.u32) matched += 4;
    ctx.r3.u64 = matched;
}
