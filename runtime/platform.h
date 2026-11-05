#pragma once
// The few things the runtime asks of the host system that Linux and Windows
// spell differently. Everything else goes through the C++ library or SDL.
#include <cstdint>

namespace platform
{
    // Processor time so far: this process's, and the busy and whole time of
    // every thread of the processor together.
    struct Load { uint64_t process = 0, busy = 0, total = 0; };
    // In bytes; 0 where the system does not say.
    struct Memory
    {
        uint64_t installed = 0, usable = 0, free = 0;
        uint64_t commitLimit = 0, committed = 0;           // the whole system's
        uint64_t processPeak = 0, processCommitted = 0;    // this process's
    };
}

extern "C"
{
    void mw2_platform_initialise();
    uint32_t mw2_platform_thread_id();
    bool mw2_platform_is_main_thread();
    void mw2_platform_set_thread_name(const char* name);
    void mw2_platform_lower_thread_priority();
    void mw2_platform_idle_thread_priority();
    void mw2_platform_print_backtrace(const char* why);
    uintptr_t mw2_platform_thread_clock();
    bool mw2_platform_thread_time(uintptr_t clock, uint64_t* nanoseconds);
    bool mw2_platform_processor_load(platform::Load* load);
    void mw2_platform_memory_state(platform::Memory* memory);
    void* mw2_platform_open_library(const char* path);
    void* mw2_platform_loaded_library(const char* name);
    void* mw2_platform_symbol(void* library, const char* name);
}

namespace platform
{
    // First thing in main, before any other thread starts: Windows sleeps in
    // 15.6 ms steps unless asked for 1 ms, and the title paces itself with
    // short sleeps; and a release build there finds the terminal it was
    // started from, if any.
    inline void Initialise() { mw2_platform_initialise(); }

    // The system's id for the calling thread, as a profiler shows it.
    inline uint32_t ThreadId() { return mw2_platform_thread_id(); }
    inline bool IsMainThread() { return mw2_platform_is_main_thread(); }
    // Up to fifteen characters are kept on Linux.
    inline void SetThreadName(const char* name) { mw2_platform_set_thread_name(name); }
    // Background work that must not take time from the frame.
    inline void LowerThreadPriority() { mw2_platform_lower_thread_priority(); }
    // Only the time nothing else wants.
    inline void IdleThreadPriority() { mw2_platform_idle_thread_priority(); }

    // The calling thread's host stack to stderr. The recompiled functions are
    // named sub_XXXXXXXX, so where symbols resolve this is the guest's stack.
    inline void PrintBacktrace(const char* why) { mw2_platform_print_backtrace(why); }

    // What a bug report says about the machine's load (report.cpp).
    // A clock of the calling thread's processor time that any thread can read;
    // 0 when there is none.
    inline uintptr_t ThreadClock() { return mw2_platform_thread_clock(); }
    // The thread's processor time so far; false once it has ended.
    inline bool ThreadTime(uintptr_t clock, uint64_t& nanoseconds) { return mw2_platform_thread_time(clock, &nanoseconds); }
    inline bool ProcessorLoad(Load& load) { return mw2_platform_processor_load(&load); }
    inline Memory MemoryState()
    {
        Memory memory;
        mw2_platform_memory_state(&memory);
        return memory;
    }

    // A shared library: loaded, or only if something already loaded it.
    inline void* OpenLibrary(const char* path) { return mw2_platform_open_library(path); }
    inline void* LoadedLibrary(const char* name) { return mw2_platform_loaded_library(name); }
    inline void* Symbol(void* library, const char* name) { return mw2_platform_symbol(library, name); }
}
