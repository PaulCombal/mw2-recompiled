#include "report.h"
#include "log.h"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <iterator>
#include <mutex>

namespace
{
    // A frame every blank is 16.7 ms; the bounds are a blank and a half, then
    // two, three and four blanks.
    constexpr double kBounds[] = { 17.5, 25.0, 34.0, 50.0 };
    constexpr size_t kBuckets = std::size(kBounds) + 1;

    struct State
    {
        std::mutex lock;
        std::chrono::steady_clock::time_point first, last;
        bool lastWasWorld = false;
        uint64_t frames = 0, worldFrames = 0;
        // Between two world frames in a row: a loading screen in between is
        // not a slow frame.
        uint64_t intervals = 0, buckets[kBuckets]{};
        double intervalMs = 0, longestMs = 0;
        // The consumer's times as they stood at the last frame, and what the
        // intervals counted above took of them.
        uint64_t spentAtLast[report::kTimeCount]{}, spentInPlay[report::kTimeCount]{};
        uint64_t costCount[report::kCostCount]{}, costMicroseconds[report::kCostCount]{},
                 costLongest[report::kCostCount]{};
        bool written = false;
    } s;

    // Written by the ring consumer between frames, outside the lock.
    std::atomic<uint64_t> g_spent[report::kTimeCount]{};
    // When the ring batch being executed began. A frame ends in the middle of
    // one, so its part up to there is counted with the frame.
    std::chrono::steady_clock::time_point g_executingSince;
    bool g_executing = false;

    std::atomic<uint64_t> g_shaders[3]{};   // by ShaderBuilt's fromCache, plus one
    std::string g_compiled = "not used: a development run";
}

void report::ShaderBuilt(int fromCache)
{
    g_shaders[std::clamp(fromCache, -1, 1) + 1].fetch_add(1, std::memory_order_relaxed);
}

void report::ShadersCompiled(std::string state)
{
    std::lock_guard lock(s.lock);
    g_compiled = std::move(state);
}

report::Timed::Timed(Time what) : time(what)
{
    if (!on) return;
    began = std::chrono::steady_clock::now();
    if (time == kExecuting) { g_executingSince = began; g_executing = true; }
}

report::Timed::~Timed()
{
    if (!on) return;
    if (time == kExecuting) g_executing = false;
    Spent(time, uint64_t(std::chrono::duration_cast<std::chrono::nanoseconds>(
                             std::chrono::steady_clock::now() - began).count()));
}

void report::Frame(bool world)
{
    if (!On()) return;
    const auto now = std::chrono::steady_clock::now();
    std::lock_guard lock(s.lock);
    if (!s.frames) s.first = now;
    s.frames++;
    uint64_t spent[kTimeCount];
    for (size_t i = 0; i < kTimeCount; i++) spent[i] = g_spent[i].load(std::memory_order_relaxed);
    if (g_executing)
        spent[kExecuting] += uint64_t(std::chrono::duration_cast<std::chrono::nanoseconds>(
                                          now - g_executingSince).count());
    if (world)
    {
        s.worldFrames++;
        if (s.lastWasWorld)
        {
            for (size_t i = 0; i < kTimeCount; i++) s.spentInPlay[i] += spent[i] - s.spentAtLast[i];
            const double ms = std::chrono::duration<double, std::milli>(now - s.last).count();
            size_t bucket = 0;
            while (bucket < std::size(kBounds) && ms > kBounds[bucket]) bucket++;
            s.buckets[bucket]++;
            s.intervals++;
            s.intervalMs += ms;
            s.longestMs = std::max(s.longestMs, ms);
        }
    }
    std::copy(std::begin(spent), std::end(spent), s.spentAtLast);
    s.lastWasWorld = world;
    s.last = now;
}

void report::Spent(Time time, uint64_t nanoseconds)
{
    g_spent[time].fetch_add(nanoseconds, std::memory_order_relaxed);
}

void report::Add(Cost cost, uint64_t microseconds)
{
    if (!On()) return;
    std::lock_guard lock(s.lock);
    s.costCount[cost]++;
    s.costMicroseconds[cost] += microseconds;
    s.costLongest[cost] = std::max(s.costLongest[cost], microseconds);
}

void report::Write()
{
    if (!On()) return;
    std::lock_guard lock(s.lock);
    if (s.written) return;
    s.written = true;
    const double seconds = s.frames ? std::chrono::duration<double>(s.last - s.first).count() : 0.0;
    LOGI("report: %llu frames in %.0f s, %llu of them of the world", (unsigned long long)s.frames, seconds,
         (unsigned long long)s.worldFrames);
    if (s.intervals)
    {
        auto share = [](uint64_t count) { return 100.0 * double(count) / double(s.intervals); };
        LOGI("report: in play, %.1f frames a second on average; the longest frame took %.0f ms",
             1000.0 * double(s.intervals) / s.intervalMs, s.longestMs);
        LOGI("report: frame times: %.1f%% within %.1f ms, %.1f%% to %.0f ms, %.1f%% to %.0f ms, %.1f%% to %.0f ms,"
             " %.1f%% longer",
             share(s.buckets[0]), kBounds[0], share(s.buckets[1]), kBounds[1], share(s.buckets[2]), kBounds[2],
             share(s.buckets[3]), kBounds[3], share(s.buckets[4]));
        auto took = [](Time time) { return double(s.spentInPlay[time]) / 1e4 / s.intervalMs; };
        LOGI("report: in play, the renderer's thread was at work %.0f%% of the time: %.0f%% waiting for the"
             " graphics chip, %.0f%% for its recorder thread",
             took(kExecuting), took(kGpuWait), took(kRecorderWait));
    }
    LOGI("report: compiled at a draw: %llu shaders in %llu ms (the longest %llu ms), %llu pipelines in %llu ms"
         " (the longest %llu ms)",
         (unsigned long long)s.costCount[kShader], (unsigned long long)(s.costMicroseconds[kShader] / 1000),
         (unsigned long long)(s.costLongest[kShader] / 1000), (unsigned long long)s.costCount[kPipeline],
         (unsigned long long)(s.costMicroseconds[kPipeline] / 1000),
         (unsigned long long)(s.costLongest[kPipeline] / 1000));
    // Every one should have come out of the launcher's cache.
    const uint64_t unknown = g_shaders[0], compiled = g_shaders[1], cached = g_shaders[2];
    LOGI("report: shaders compiled before play by the launcher: %s", g_compiled.c_str());
    if (unknown)
        LOGI("report: %llu shaders built in this run, %llu of them not in the launcher's (the driver says so for %llu)",
             (unsigned long long)(unknown + compiled + cached), (unsigned long long)compiled,
             (unsigned long long)(compiled + cached));
    else
        LOGI("report: %llu shaders built in this run, %llu of them not in the launcher's",
             (unsigned long long)(compiled + cached), (unsigned long long)compiled);
}
