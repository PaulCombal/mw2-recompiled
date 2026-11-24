#pragma once

// src/runtime/crash.rs and run.rs, under the names src/runtime/exports.rs gives them.
extern "C"
{
    void mw2_crash_register_thread(const char* name);
    void mw2_crash_request_exit(const char* why);
}

namespace crash
{
    // Every thread registers itself so the watchdog can show where all of them
    // are, not just the one that happened to be running. A stall is nearly
    // always one thread waiting on another.
    inline void RegisterThread(const char* name) { mw2_crash_register_thread(name); }

    // The guest never returns, so nothing else can end the process tidily.
    // Safe to call from any thread, including one the shutdown will join.
    inline void RequestExit(const char* why) { mw2_crash_request_exit(why); }
}
