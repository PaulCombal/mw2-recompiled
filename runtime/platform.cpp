#include "platform.h"

#include <cstdio>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <timeapi.h>
#include <psapi.h>
#include <string>
#else
#include <cstdlib>
#include <cstring>
#include <ctime>
#include <dlfcn.h>
#include <execinfo.h>
#include <pthread.h>
#include <sched.h>
#include <sys/resource.h>
#include <unistd.h>
#endif

#ifdef _WIN32

#include <cmath>

// C23's round-half-to-even, which the recompiled vector rounding reaches
// through simde and the UCRT does not have. Whatever the rounding mode: a tie
// goes to the even neighbour, anything else to the nearer one.
extern "C" float roundevenf(float x)
{
    const float nearest = std::round(x);   // ties away from zero
    if (std::fabs(nearest - x) != 0.5f) return nearest;
    return 2.0f * std::round(x * 0.5f);
}

namespace { DWORD g_mainThread = 0; }

void platform::Initialise()
{
    g_mainThread = GetCurrentThreadId();
    timeBeginPeriod(1);
    // A player's build has no console window of its own. Started from a
    // terminal, it writes there, as a console program would.
    if (!GetConsoleWindow() && AttachConsole(ATTACH_PARENT_PROCESS))
    {
        std::freopen("CONOUT$", "w", stdout);
        std::freopen("CONOUT$", "w", stderr);
    }
}

uint32_t platform::ThreadId() { return uint32_t(GetCurrentThreadId()); }
bool platform::IsMainThread() { return GetCurrentThreadId() == g_mainThread; }

void platform::SetThreadName(const char* name)
{
    wchar_t wide[64];
    const int n = MultiByteToWideChar(CP_UTF8, 0, name, -1, wide, 64);
    if (n <= 0) return;
    // Windows 10 1607 and later; looked up, so older systems just go without.
    using SetDescription = HRESULT(WINAPI*)(HANDLE, PCWSTR);
    static const auto set = reinterpret_cast<SetDescription>(
        reinterpret_cast<void*>(GetProcAddress(GetModuleHandleW(L"kernel32.dll"), "SetThreadDescription")));
    if (set) set(GetCurrentThread(), wide);
}

void platform::LowerThreadPriority() { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL); }
void platform::IdleThreadPriority() { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_IDLE); }

// No symbols without a PDB: each frame as its offset in the executable, which
// `llvm-symbolizer --obj=mw2.exe` turns back into a name on the build machine.
void platform::PrintBacktrace(const char* why)
{
    void* frames[64];
    const USHORT n = CaptureStackBackTrace(0, 64, frames, nullptr);
    const auto module = reinterpret_cast<uintptr_t>(GetModuleHandleW(nullptr));
    std::fprintf(stderr, "\n[E] ---- %s (%u frames) ----\n", why, unsigned(n));
    for (USHORT i = 0; i < n; i++)
    {
        const auto at = reinterpret_cast<uintptr_t>(frames[i]);
        HMODULE owner = nullptr;
        GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                           reinterpret_cast<LPCWSTR>(frames[i]), &owner);
        if (reinterpret_cast<uintptr_t>(owner) == module)
            std::fprintf(stderr, "[E]   exe+0x%llx\n", (unsigned long long)(at - module));
        else
            std::fprintf(stderr, "[E]   %p\n", frames[i]);
    }
    std::fprintf(stderr, "[E] --------------------------------\n");
    std::fflush(stderr);
}

void* platform::OpenLibrary(const char* path)
{
    const int n = MultiByteToWideChar(CP_UTF8, 0, path, -1, nullptr, 0);
    std::wstring wide(size_t(n > 0 ? n : 1), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, path, -1, wide.data(), n);
    // The library's own folder is searched for what it loads in turn.
    return LoadLibraryExW(wide.c_str(), nullptr, LOAD_WITH_ALTERED_SEARCH_PATH);
}

uintptr_t platform::ThreadClock()
{
    HANDLE self = nullptr;
    // A handle of its own: GetCurrentThread's means whichever thread asks.
    if (!DuplicateHandle(GetCurrentProcess(), GetCurrentThread(), GetCurrentProcess(), &self,
                         THREAD_QUERY_LIMITED_INFORMATION, FALSE, 0))
        return 0;
    return reinterpret_cast<uintptr_t>(self);
}

namespace
{
    uint64_t Nanoseconds(FILETIME time)
    {
        return ((uint64_t(time.dwHighDateTime) << 32) | time.dwLowDateTime) * 100;
    }
}

bool platform::ThreadTime(uintptr_t clock, uint64_t& nanoseconds)
{
    FILETIME created, exited, kernel, user;
    if (!clock || !GetThreadTimes(reinterpret_cast<HANDLE>(clock), &created, &exited, &kernel, &user))
        return false;
    nanoseconds = Nanoseconds(kernel) + Nanoseconds(user);
    return true;
}

bool platform::ProcessorLoad(Load& load)
{
    FILETIME created, exited, kernel, user, idle;
    if (!GetProcessTimes(GetCurrentProcess(), &created, &exited, &kernel, &user)) return false;
    load.process = Nanoseconds(kernel) + Nanoseconds(user);
    // The system's kernel time has its idle time in it.
    if (!GetSystemTimes(&idle, &kernel, &user)) return false;
    load.total = Nanoseconds(kernel) + Nanoseconds(user);
    load.busy = load.total - Nanoseconds(idle);
    return true;
}

platform::Memory platform::MemoryState()
{
    Memory memory;
    ULONGLONG kilobytes = 0;
    if (GetPhysicallyInstalledSystemMemory(&kilobytes)) memory.installed = uint64_t(kilobytes) << 10;
    MEMORYSTATUSEX status{ sizeof(status) };
    if (GlobalMemoryStatusEx(&status))
    {
        memory.usable = status.ullTotalPhys;
        memory.free = status.ullAvailPhys;
        memory.commitLimit = status.ullTotalPageFile;
        memory.committed = status.ullTotalPageFile - status.ullAvailPageFile;
    }
    PROCESS_MEMORY_COUNTERS counters{ sizeof(counters) };
    if (K32GetProcessMemoryInfo(GetCurrentProcess(), &counters, sizeof(counters)))
    {
        memory.processPeak = counters.PeakWorkingSetSize;
        memory.processCommitted = counters.PeakPagefileUsage;
    }
    return memory;
}

void* platform::LoadedLibrary(const char* name) { return GetModuleHandleA(name); }

void* platform::Symbol(void* library, const char* name)
{
    return reinterpret_cast<void*>(GetProcAddress(static_cast<HMODULE>(library), name));
}

#else

void platform::Initialise() {}
uint32_t platform::ThreadId() { return uint32_t(gettid()); }
bool platform::IsMainThread() { return gettid() == getpid(); }

void platform::SetThreadName(const char* name)
{
    char shortName[16];
    std::snprintf(shortName, sizeof shortName, "%s", name);
    pthread_setname_np(pthread_self(), shortName);
}

void platform::LowerThreadPriority() { setpriority(PRIO_PROCESS, static_cast<id_t>(gettid()), 10); }

void platform::IdleThreadPriority()
{
    sched_param none{};
    if (pthread_setschedparam(pthread_self(), SCHED_IDLE, &none) != 0) LowerThreadPriority();
}

void platform::PrintBacktrace(const char* why)
{
    void* frames[64];
    const int n = backtrace(frames, 64);
    std::fprintf(stderr, "\n[E] ---- %s (%d frames) ----\n", why, n);
    std::fflush(stderr);
    backtrace_symbols_fd(frames, n, 2);
    std::fprintf(stderr, "[E] --------------------------------\n");
    std::fflush(stderr);
}

uintptr_t platform::ThreadClock()
{
    clockid_t clock;
    if (pthread_getcpuclockid(pthread_self(), &clock) != 0) return 0;
    // Shifted past 0, which says there is none.
    return uintptr_t(uint32_t(clock)) + 1;
}

bool platform::ThreadTime(uintptr_t clock, uint64_t& nanoseconds)
{
    timespec time;
    if (!clock || clock_gettime(clockid_t(uint32_t(clock - 1)), &time) != 0) return false;
    nanoseconds = uint64_t(time.tv_sec) * 1000000000u + uint64_t(time.tv_nsec);
    return true;
}

bool platform::ProcessorLoad(Load& load)
{
    timespec time;
    if (clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &time) != 0) return false;
    load.process = uint64_t(time.tv_sec) * 1000000000u + uint64_t(time.tv_nsec);
    // The first line sums every processor thread, in ticks of USER_HZ.
    std::FILE* stat = std::fopen("/proc/stat", "r");
    if (!stat) return false;
    unsigned long long user = 0, nice = 0, system = 0, idle = 0, iowait = 0, irq = 0, softirq = 0, steal = 0;
    const int read = std::fscanf(stat, "cpu %llu %llu %llu %llu %llu %llu %llu %llu", &user, &nice, &system, &idle,
                                 &iowait, &irq, &softirq, &steal);
    std::fclose(stat);
    if (read < 4) return false;
    const uint64_t tick = 1000000000u / uint64_t(sysconf(_SC_CLK_TCK));
    load.total = (user + nice + system + idle + iowait + irq + softirq + steal) * tick;
    load.busy = load.total - (idle + iowait) * tick;
    return true;
}

namespace
{
    // A "Name: N kB" line of a /proc file, in bytes.
    uint64_t Kilobytes(const char* path, const char* name)
    {
        std::FILE* file = std::fopen(path, "r");
        if (!file) return 0;
        const size_t length = std::strlen(name);
        char line[256];
        uint64_t bytes = 0;
        while (std::fgets(line, sizeof line, file))
            if (!std::strncmp(line, name, length) && line[length] == ':')
            {
                bytes = std::strtoull(line + length + 1, nullptr, 10) << 10;
                break;
            }
        std::fclose(file);
        return bytes;
    }
}

platform::Memory platform::MemoryState()
{
    Memory memory;
    memory.usable = Kilobytes("/proc/meminfo", "MemTotal");
    memory.free = Kilobytes("/proc/meminfo", "MemAvailable");
    memory.processPeak = Kilobytes("/proc/self/status", "VmHWM");
    return memory;
}

void* platform::OpenLibrary(const char* path) { return dlopen(path, RTLD_NOW | RTLD_LOCAL); }
void* platform::LoadedLibrary(const char* name) { return dlopen(name, RTLD_NOW | RTLD_NOLOAD); }
void* platform::Symbol(void* library, const char* name) { return dlsym(library, name); }

#endif
