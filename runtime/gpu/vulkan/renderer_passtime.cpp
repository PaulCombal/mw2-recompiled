// MW2_TIME_PASSES: where the GPU's time goes inside a frame (renderer_state.h).
//
// A timestamp is written at the bottom of the pipe after each rendering
// instance ends and before the next begins: the GPU writes it once everything
// before it is done, so the time between two is the work between them. A
// pass's barrier out waits for the pass, so passes do not overlap much and the
// split is close to what each costs. Uploads and copies between passes go to
// "between passes".
#include "renderer_state.h"
#include "../../log.h"

#include <algorithm>
#include <cstdarg>
#include <chrono>
#include <cstdio>
#include <map>
#include <string>
#include <vector>

#ifdef MW2_HAVE_VULKAN

namespace vk::renderer::detail
{
    namespace
    {
        constexpr uint32_t kMarks = 4096;   // a slot's, a submission at most
        constexpr uint16_t kBetween = 0;

        struct Label
        {
            std::string name;
            uint64_t ticks = 0, instances = 0, draws = 0;
        };

        struct Times
        {
            bool on = false;
            std::chrono::steady_clock::time_point from;
            VkQueryPool pool = VK_NULL_HANDLE;
            double nanosecondsPerTick = 1.0;
            uint64_t mask = ~0ull;
            // What each mark of a slot's submission closes, and the draws
            // made since the mark before.
            std::vector<uint16_t> labels[kFrameSlots];
            std::vector<uint32_t> draws[kFrameSlots];
            uint32_t slot = 0;
            uint64_t drawsAtMark = 0;
            std::vector<Label> table;
            std::map<std::string, uint16_t> byName;
            uint16_t open = kBetween;   // the label of the pass open, kBetween when none
            bool counting = false;
            uint64_t framesAtStart = 0, submissions = 0;
            std::vector<uint64_t> results;
        } t;

        uint16_t LabelFor(const std::string& name)
        {
            const auto [at, added] = t.byName.emplace(name, uint16_t(t.table.size()));
            if (added) t.table.push_back(Label{ name });
            return at->second;
        }

        void Mark(uint16_t label)
        {
            std::vector<uint16_t>& labels = t.labels[t.slot];
            if (labels.size() >= kMarks) return;
            const VkQueryPool pool = t.pool;
            const uint32_t query = t.slot * kMarks + uint32_t(labels.size());
            labels.push_back(label);
            t.draws[t.slot].push_back(uint32_t(g.draws - t.drawsAtMark));
            t.drawsAtMark = g.draws;
            Record([=](VkCommandBuffer command) {
                vkCmdWriteTimestamp(command, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, pool, query);
            });
        }

        // The slot's last submission is done: its marks are all written.
        void Collect(uint32_t slot)
        {
            std::vector<uint16_t>& labels = t.labels[slot];
            std::vector<uint32_t>& draws = t.draws[slot];
            if (labels.size() > 1 && t.counting)
            {
                t.results.resize(labels.size());
                if (vkGetQueryPoolResults(g.device, t.pool, slot * kMarks, uint32_t(labels.size()),
                                          t.results.size() * sizeof(uint64_t), t.results.data(), sizeof(uint64_t),
                                          VK_QUERY_RESULT_64_BIT) == VK_SUCCESS)
                {
                    for (size_t i = 1; i < labels.size(); i++)
                    {
                        Label& label = t.table[labels[i]];
                        label.ticks += (t.results[i] - t.results[i - 1]) & t.mask;
                        label.draws += draws[i];
                        if (labels[i] != kBetween) label.instances++;
                    }
                    t.submissions++;
                }
            }
            labels.clear();
            draws.clear();
        }
    }

    void StartPassTimes()
    {
        const uint64_t seconds = diag::Number("MW2_TIME_PASSES", UINT64_MAX);
        if (seconds == UINT64_MAX) return;
        uint32_t families = 0;
        vkGetPhysicalDeviceQueueFamilyProperties(g.physical, &families, nullptr);
        std::vector<VkQueueFamilyProperties> properties(families);
        vkGetPhysicalDeviceQueueFamilyProperties(g.physical, &families, properties.data());
        VkPhysicalDeviceProperties device{};
        vkGetPhysicalDeviceProperties(g.physical, &device);
        const uint32_t family = vk::pipeline::QueueFamily();
        const uint32_t bits = family < families ? properties[family].timestampValidBits : 0;
        if (!bits) { LOGW("renderer: MW2_TIME_PASSES: the queue keeps no time"); return; }
        t.mask = bits >= 64 ? ~0ull : (1ull << bits) - 1;
        t.nanosecondsPerTick = device.limits.timestampPeriod;
        VkQueryPoolCreateInfo queries{ VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO };
        queries.queryType = VK_QUERY_TYPE_TIMESTAMP;
        queries.queryCount = kFrameSlots * kMarks;
        if (vkCreateQueryPool(g.device, &queries, nullptr, &t.pool) != VK_SUCCESS) return;
        t.table.push_back(Label{ "between passes: copies, resolves, clears, uploads" });
        t.from = std::chrono::steady_clock::now() + std::chrono::seconds(seconds);
        t.on = true;
        LOGI("renderer: timing the GPU's passes from %llu s into the run (MW2_TIME_PASSES)",
             (unsigned long long)seconds);
    }

    void PassTimesBegin(uint32_t slot)
    {
        if (!t.on) return;
        Collect(slot);
        if (!t.counting && std::chrono::steady_clock::now() >= t.from)
        {
            t.counting = true;
            t.framesAtStart = g.frames;
        }
        t.slot = slot;
        t.open = kBetween;
        const VkQueryPool pool = t.pool;
        Record([=](VkCommandBuffer command) { vkCmdResetQueryPool(command, pool, slot * kMarks, kMarks); });
        Mark(kBetween);
    }

    void PassTimesOpen(const Target& colour, uint32_t samples)
    {
        if (!t.on) return;
        Mark(kBetween);
        char name[96];
        std::snprintf(name, sizeof name, "%ux%u %ux %s", colour.width * colour.scale, colour.height * colour.scale,
                      samples, gpu::ColorTargetFormatName(colour.guestFormat) ?: "?");
        t.open = LabelFor(name);
    }

    void PassTimesClose()
    {
        if (!t.on || t.open == kBetween) return;
        Mark(t.open);
        t.open = kBetween;
    }

    void PassTimesBetween()
    {
        if (t.on) Mark(kBetween);
    }

    void PassTimesMark(const char* format, ...)
    {
        if (!t.on) return;
        char name[96];
        va_list arguments;
        va_start(arguments, format);
        std::vsnprintf(name, sizeof name, format, arguments);
        va_end(arguments);
        Mark(LabelFor(name));
    }

    // With the device idle.
    void StopPassTimes()
    {
        if (!t.on) return;
        for (uint32_t slot = 0; slot < kFrameSlots; slot++) Collect(slot);
        const uint64_t frames = std::max<uint64_t>(1, g.frames - t.framesAtStart);
        uint64_t all = 0;
        for (const Label& label : t.table) all += label.ticks;
        std::vector<const Label*> order;
        for (const Label& label : t.table) if (label.ticks) order.push_back(&label);
        std::sort(order.begin(), order.end(), [](const Label* a, const Label* b) { return a->ticks > b->ticks; });
        auto ms = [](uint64_t ticks) { return double(ticks) * t.nanosecondsPerTick / 1e6; };
        LOGI("renderer: the GPU's passes over %llu frames (%llu submissions): %.2f ms a frame",
             (unsigned long long)frames, (unsigned long long)t.submissions, ms(all) / double(frames));
        for (const Label* label : order)
            LOGI("renderer:   %6.2f ms a frame %5.1f%%  %6.1f passes %7.1f draws  %s", ms(label->ticks) / double(frames),
                 100.0 * double(label->ticks) / double(std::max<uint64_t>(all, 1)),
                 double(label->instances) / double(frames), double(label->draws) / double(frames), label->name.c_str());
        vkDestroyQueryPool(g.device, t.pool, nullptr);
        t.pool = VK_NULL_HANDLE;
        t.on = false;
    }
}

#endif
