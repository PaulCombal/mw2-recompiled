#include "gpu_time.h"
#include "pipeline.h"
#include "../../log.h"
#include "../../report.h"

#include <atomic>
#include <mutex>
#include <vector>

#ifdef MW2_HAVE_VULKAN

namespace
{
    // Pairs of queries, one a submission, taken in turn. A frame is a few
    // submissions and they are collected every frame, so the ring is never
    // close to full; a submission that finds it full goes untimed.
    constexpr uint32_t kPairs = 64;

    struct State
    {
        std::atomic<bool> on{ false };
        VkDevice device = VK_NULL_HANDLE;
        VkQueryPool pool = VK_NULL_HANDLE;
        VkCommandPool commands = VK_NULL_HANDLE;
        // Recorded once: the first resets its pair and stamps the start, the
        // second stamps the end.
        VkCommandBuffer begins[kPairs]{}, ends[kPairs]{};
        double nanosecondsPerTick = 1.0;
        uint64_t mask = ~0ull;            // the bits of a timestamp that count
        std::mutex lock;
        uint64_t taken = 0;               // pairs handed to submissions, under the lock
        std::atomic<uint64_t> collected{ 0 };
        std::atomic<bool> live[kPairs]{}; // false for a submission the queue refused
        // A pair's results from its last round stay readable until the GPU
        // reaches its reset; seeing them again is "not written yet".
        uint64_t seen[kPairs][2]{};
        uint64_t lastEnd = 0;
        bool anyEnd = false;
        std::vector<VkCommandBuffer> batch;   // under the queue's lock
    } g;

    // Whether timestamp `a` is before `b`, both read through the mask: they
    // are never half the counter's range apart.
    bool Before(uint64_t a, uint64_t b) { return ((b - a) & g.mask) - 1 < (g.mask >> 1); }
}

void vk::gpu_time::Start(VkPhysicalDevice physical, VkDevice device, VkQueue queue, uint32_t family)
{
    if (!report::On()) return;
    uint32_t families = 0;
    vkGetPhysicalDeviceQueueFamilyProperties(physical, &families, nullptr);
    std::vector<VkQueueFamilyProperties> properties(families);
    vkGetPhysicalDeviceQueueFamilyProperties(physical, &families, properties.data());
    VkPhysicalDeviceProperties limits{};
    vkGetPhysicalDeviceProperties(physical, &limits);
    const uint32_t bits = family < families ? properties[family].timestampValidBits : 0;
    if (!bits || limits.limits.timestampPeriod <= 0.0f)
    {
        LOGI("report: the graphics chip does not keep time on its queue; its busy time is not known");
        return;
    }
    g.device = device;
    g.mask = bits >= 64 ? ~0ull : (1ull << bits) - 1;
    g.nanosecondsPerTick = limits.limits.timestampPeriod;

    VkQueryPoolCreateInfo queries{ VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO };
    queries.queryType = VK_QUERY_TYPE_TIMESTAMP;
    queries.queryCount = 2 * kPairs;
    VkCommandPoolCreateInfo pool{ VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO };
    pool.queueFamilyIndex = family;
    if (vkCreateQueryPool(device, &queries, nullptr, &g.pool) != VK_SUCCESS ||
        vkCreateCommandPool(device, &pool, nullptr, &g.commands) != VK_SUCCESS)
    {
        Stop();
        return;
    }
    VkCommandBufferAllocateInfo allocate{ VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO };
    allocate.commandPool = g.commands;
    allocate.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY;
    allocate.commandBufferCount = kPairs;
    VkCommandBuffer reset = VK_NULL_HANDLE;
    if (vkAllocateCommandBuffers(device, &allocate, g.begins) != VK_SUCCESS ||
        vkAllocateCommandBuffers(device, &allocate, g.ends) != VK_SUCCESS ||
        (allocate.commandBufferCount = 1, vkAllocateCommandBuffers(device, &allocate, &reset)) != VK_SUCCESS)
    {
        Stop();
        return;
    }
    // Resubmitted every kPairs submissions, by when the last round is done.
    VkCommandBufferBeginInfo begin{ VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO };
    begin.flags = VK_COMMAND_BUFFER_USAGE_SIMULTANEOUS_USE_BIT;
    for (uint32_t pair = 0; pair < kPairs; pair++)
    {
        vkBeginCommandBuffer(g.begins[pair], &begin);
        vkCmdResetQueryPool(g.begins[pair], g.pool, 2 * pair, 2);
        vkCmdWriteTimestamp(g.begins[pair], VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, g.pool, 2 * pair);
        vkEndCommandBuffer(g.begins[pair]);
        vkBeginCommandBuffer(g.ends[pair], &begin);
        vkCmdWriteTimestamp(g.ends[pair], VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, g.pool, 2 * pair + 1);
        vkEndCommandBuffer(g.ends[pair]);
    }
    // Every query starts reset, so a pair not yet written reads as unavailable.
    VkCommandBufferBeginInfo once{ VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO };
    once.flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    vkBeginCommandBuffer(reset, &once);
    vkCmdResetQueryPool(reset, g.pool, 0, 2 * kPairs);
    vkEndCommandBuffer(reset);
    VkSubmitInfo submit{ VK_STRUCTURE_TYPE_SUBMIT_INFO };
    submit.commandBufferCount = 1;
    submit.pCommandBuffers = &reset;
    bool done;
    {
        std::lock_guard lock(vk::pipeline::QueueMutex());
        done = vkQueueSubmit(queue, 1, &submit, VK_NULL_HANDLE) == VK_SUCCESS && vkQueueWaitIdle(queue) == VK_SUCCESS;
    }
    if (!done)
    {
        Stop();
        return;
    }
    vkFreeCommandBuffers(device, g.commands, 1, &reset);
    g.on.store(true, std::memory_order_release);
}

void vk::gpu_time::Stop()
{
    if (g.on.load(std::memory_order_acquire)) Collect();
    g.on.store(false, std::memory_order_release);
    if (g.commands) vkDestroyCommandPool(g.device, g.commands, nullptr);
    if (g.pool) vkDestroyQueryPool(g.device, g.pool, nullptr);
    g.commands = VK_NULL_HANDLE;
    g.pool = VK_NULL_HANDLE;
}

VkResult vk::gpu_time::Submit(VkQueue queue, const VkSubmitInfo& submit, VkFence fence)
{
    if (!g.on.load(std::memory_order_acquire)) return vkQueueSubmit(queue, 1, &submit, fence);
    uint32_t pair;
    {
        std::lock_guard lock(g.lock);
        if (g.taken - g.collected.load(std::memory_order_acquire) >= kPairs)
            return vkQueueSubmit(queue, 1, &submit, fence);
        pair = uint32_t(g.taken % kPairs);
        g.live[pair].store(true, std::memory_order_relaxed);
        g.taken++;
    }
    g.batch.clear();
    g.batch.push_back(g.begins[pair]);
    g.batch.insert(g.batch.end(), submit.pCommandBuffers, submit.pCommandBuffers + submit.commandBufferCount);
    g.batch.push_back(g.ends[pair]);
    VkSubmitInfo timed = submit;
    timed.commandBufferCount = uint32_t(g.batch.size());
    timed.pCommandBuffers = g.batch.data();
    const VkResult result = vkQueueSubmit(queue, 1, &timed, fence);
    if (result != VK_SUCCESS) g.live[pair].store(false, std::memory_order_release);
    return result;
}

void vk::gpu_time::Collect()
{
    if (!g.on.load(std::memory_order_acquire)) return;
    uint64_t taken;
    {
        std::lock_guard lock(g.lock);
        taken = g.taken;
    }
    uint64_t collected = g.collected.load(std::memory_order_relaxed), busy = 0;
    for (; collected < taken; collected++)
    {
        const uint32_t pair = uint32_t(collected % kPairs);
        if (!g.live[pair].load(std::memory_order_acquire)) continue;
        // Each query's value and then whether it is there yet.
        uint64_t results[2][2]{};
        const VkResult read = vkGetQueryPoolResults(g.device, g.pool, 2 * pair, 2, sizeof results, results,
                                                    sizeof results[0],
                                                    VK_QUERY_RESULT_64_BIT | VK_QUERY_RESULT_WITH_AVAILABILITY_BIT);
        if ((read != VK_SUCCESS && read != VK_NOT_READY) || !results[0][1] || !results[1][1]) break;
        const uint64_t start = results[0][0] & g.mask, end = results[1][0] & g.mask;
        if (start == g.seen[pair][0] && end == g.seen[pair][1]) break;
        g.seen[pair][0] = start;
        g.seen[pair][1] = end;
        // Submissions overlap -- the start is stamped as the command processor
        // reaches it, which can be while the last one's draws are still on --
        // so only the part past the last end is new.
        const uint64_t from = g.anyEnd && Before(start, g.lastEnd) ? g.lastEnd : start;
        if (Before(from, end)) busy += (end - from) & g.mask;
        if (!g.anyEnd || Before(g.lastEnd, end)) g.lastEnd = end;
        g.anyEnd = true;
    }
    g.collected.store(collected, std::memory_order_release);
    if (busy) report::Spent(report::kGpuBusy, uint64_t(double(busy) * g.nanosecondsPerTick));
}

#endif
