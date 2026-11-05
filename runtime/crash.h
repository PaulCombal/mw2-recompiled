#pragma once

extern "C"
{
    void mw2_crash_install();
    void mw2_crash_register_thread(const char* name);
    const char* mw2_crash_current_thread_name();
    void mw2_crash_unregister_thread();
    // The run's end: src/runtime/run.rs, or main.cpp in the C++ build.
    void mw2_crash_request_exit(const char* why);
}

namespace crash
{
    inline void Install() { mw2_crash_install(); }

    // Every guest thread registers itself so the watchdog can show where all of
    // them are, not just the one that happened to be running. A stall is nearly
    // always one thread waiting on another.
    inline void RegisterThread(const char* name) { mw2_crash_register_thread(name); }
    // The label this thread registered, or null.
    inline const char* CurrentThreadName() { return mw2_crash_current_thread_name(); }
    inline void UnregisterThread() { mw2_crash_unregister_thread(); }

    // Defined by the C++ build's main.cpp; called before aborting, so a run that
    // ends on a fault still prints the end-of-run reports.
    void OnFault();

    // The guest never returns, so nothing else can end the process tidily.
    // Safe to call from any thread, including one the shutdown will join.
    inline void RequestExit(const char* why) { mw2_crash_request_exit(why); }
}
