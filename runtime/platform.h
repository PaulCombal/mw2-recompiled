#pragma once
// The few things the runtime asks of the host system that Linux and Windows
// spell differently. Everything else goes through the C++ library or SDL.
#include <cstdint>
#include <thread>
#ifdef __APPLE__
#include <functional>
#include <pthread.h>
#include <type_traits>
#include <utility>
#endif

namespace platform
{
    // First thing in main, before any other thread starts: Windows sleeps in
    // 15.6 ms steps unless asked for 1 ms, and the title paces itself with
    // short sleeps; and a release build there finds the terminal it was
    // started from, if any.
    void Initialise();

    // The system's id for the calling thread, as a profiler shows it.
    uint32_t ThreadId();
    bool IsMainThread();
    // Up to fifteen characters are kept on Linux.
    void SetThreadName(const char* name);
    // Background work that must not take time from the frame.
    void LowerThreadPriority();
    // Only the time nothing else wants.
    void IdleThreadPriority();

    // The calling thread's host stack to stderr. The recompiled functions are
    // named sub_XXXXXXXX, so where symbols resolve this is the guest's stack.
    void PrintBacktrace(const char* why);

    // What a bug report says about the machine's load (report.cpp).
    // A clock of the calling thread's processor time that any thread can read;
    // 0 when there is none.
    uintptr_t ThreadClock();
    // The thread's processor time so far; false once it has ended.
    bool ThreadTime(uintptr_t clock, uint64_t& nanoseconds);
    // Processor time so far: this process's, and the busy and whole time of
    // every thread of the processor together.
    struct Load { uint64_t process = 0, busy = 0, total = 0; };
    bool ProcessorLoad(Load& load);
    // In bytes; 0 where the system does not say.
    struct Memory
    {
        uint64_t installed = 0, usable = 0, free = 0;
        uint64_t commitLimit = 0, committed = 0;           // the whole system's
        uint64_t processPeak = 0, processCommitted = 0;    // this process's
    };
    Memory MemoryState();

    // A thread that runs recompiled code, or a driver's, which nests deep on the
    // host's stack: Windows gives the stack the executable asks for (16 MB) and
    // Linux 8 MB, but a thread macOS starts gets 512 KB. So there it is a pthread
    // with a stack of 16 MB (memory it only touches as it uses it); elsewhere it
    // is std::thread itself.
#ifdef __APPLE__
    class Thread
    {
    public:
        Thread() = default;
        template <typename F, typename = std::enable_if_t<!std::is_same_v<std::decay_t<F>, Thread>>>
        explicit Thread(F&& function) { Start(std::function<void()>(std::forward<F>(function))); }
        Thread(Thread&& other) noexcept : thread_(other.thread_), started_(other.started_) { other.started_ = false; }
        Thread& operator=(Thread&& other) noexcept;
        Thread(const Thread&) = delete;
        Thread& operator=(const Thread&) = delete;
        ~Thread();   // ends the process if still joinable, as std::thread does

        bool joinable() const { return started_; }
        void join();
        void detach();

    private:
        void Start(std::function<void()> function);
        pthread_t thread_{};
        bool started_ = false;
    };
#else
    using Thread = std::thread;
#endif

    // The program's entry. On macOS the window system (SDL's windows and events)
    // works on the process's first thread alone, so there `entry` runs on a
    // thread of its own with a large stack, the first thread serves
    // RunOnMainThread's jobs, and the process ends with `entry`'s result;
    // elsewhere it is just called.
    int RunMain(int (*entry)(int, char**), int argc, char** argv);
#ifdef __APPLE__
    // Runs `job` on the process's first thread (which does so until the process
    // ends), and returns at once. Only after RunMain.
    void RunOnMainThread(void (*job)());
#endif

    // A shared library: loaded, or only if something already loaded it.
    void* OpenLibrary(const char* path);
    void* LoadedLibrary(const char* name);
    void* Symbol(void* library, const char* name);
}
