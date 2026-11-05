#pragma once
// MW2_REPORT=1: what a bug report needs to say about a run, in every build.
//
// The launcher's REPORT A BUG starts the game with it. The log then names the
// graphics driver, and ends -- at exit or at a fault -- with how the run
// went: the frame rate, how the frame times spread, where the renderer's
// thread spent play, and what the renderer compiled in the middle of it. Off,
// every call here is one load of a flag.
#include "env.h"
#include <string>
#include <cstdint>

extern "C"
{
    void mw2_report_frame(bool world);
    void mw2_report_add(int cost, uint64_t microseconds);
    void mw2_report_spent(int time, uint64_t nanoseconds);
    void mw2_report_shader_built(int fromCache);
    void mw2_report_thread_started(const char* name);
    void mw2_report_thread_ended();
    void mw2_report_shaders_compiled(const char* state);
    uint64_t mw2_report_timed_begin(int time);
    void mw2_report_timed_end(int time, uint64_t began);
    void mw2_report_write();
}

namespace report
{
    inline bool On()
    {
        static const bool on = env::Flag("MW2_REPORT");
        return on;
    }
    // The renderer finished a frame; `world` when it drew the world and not
    // only a menu or a loading screen.
    inline void Frame(bool world) { if (On()) mw2_report_frame(world); }
    // Work done at a draw that play had to wait for.
    enum Cost { kShader, kPipeline, kCostCount };
    inline void Add(Cost cost, uint64_t microseconds) { if (On()) mw2_report_add(cost, microseconds); }
    // Where the ring consumer's time goes, which says what holds a slow run
    // back: the thread itself when it is busy all the time, the graphics chip
    // when the busy time is waiting for it, the title when it is neither.
    // kExecuting has the others in it. All are timed on the consumer's
    // thread, where report::Frame is called from, but kGpuBusy: the time the
    // graphics chip itself was at work, by its own clock (gpu_time.h).
    enum Time { kExecuting, kRecorderWait, kGpuWait, kTextures, kCompiling, kGpuBusy, kTimeCount };
    inline void Spent(Time time, uint64_t nanoseconds) { mw2_report_spent(time, nanoseconds); }
    // A shader the renderer built, and whether the driver found it in the
    // pipeline cache the launcher compiled every shader of the game into:
    // 1 it did, 0 it compiled it, -1 it does not say. Any thread.
    inline void ShaderBuilt(int fromCache) { mw2_report_shader_built(fromCache); }
    // A thread of ours, so the report can say what each one took of the
    // processor in play. Called on the thread itself (crash::RegisterThread).
    inline void ThreadStarted(const char* name) { mw2_report_thread_started(name); }
    inline void ThreadEnded() { mw2_report_thread_ended(); }
    // What the launcher's compile pass is to this run (shader_marker.h).
    inline void ShadersCompiled(const std::string& state) { mw2_report_shaders_compiled(state.c_str()); }
    struct Timed
    {
        const Time time;
        const bool on = On();
        uint64_t began = 0;
        explicit Timed(Time what) : time(what)
        {
            if (on) began = mw2_report_timed_begin(time);
        }
        ~Timed()
        {
            if (on) mw2_report_timed_end(time, began);
        }
    };
    inline void Write() { mw2_report_write(); }
}
