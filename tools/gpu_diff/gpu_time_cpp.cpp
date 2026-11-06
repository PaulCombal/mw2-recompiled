// runtime/gpu/vulkan/gpu_time.cpp as it is, compiled into the rig: Start, Submit and
// Collect on stand-in Vulkan calls (presenter_stubs.cpp), the report's call caught.
#include <atomic>
#include <cstring>
#include <mutex>
#include <new>
#include <vector>

#include "presenter_rig.h"

#include "report.h"

namespace rig_report
{
    enum Time { kGpuBusy = 5 };
    inline uint64_t spent = 0;
    inline uint32_t spends = 0;
    inline bool On() { return true; }
    inline void Spent(Time time, uint64_t nanoseconds)
    {
        if (time == kGpuBusy) spent += nanoseconds;
        spends++;
    }
}

#define vk vkg
#define report rig_report
#include "../../runtime/gpu/vulkan/gpu_time.cpp"
#undef report
#undef vk

std::mutex& vkg::pipeline::QueueMutex()
{
    static std::mutex queue;
    return queue;
}

namespace
{
    template <class T> T Fake(uint64_t value) { return reinterpret_cast<T>(uintptr_t(value)); }
}

extern "C"
{
    // Start on a device whose families keep `bits` of time: whether it is on, its mask
    // and nanoseconds per tick.
    bool rig_cpp_gpu_start(const uint32_t* bits, uint32_t families, uint32_t family, float period, uint64_t* mask,
                           double* nanosecondsPerTick)
    {
        g.~State();
        new (&g) State{};
        rig::s.familyBits.assign(bits, bits + families);
        rig::s.timestampPeriod = period;
        rig::s.queueSubmit = VK_SUCCESS;
        vkg::gpu_time::Start(Fake<VkPhysicalDevice>(0xA000), Fake<VkDevice>(0xA001), Fake<VkQueue>(0xA002), family);
        *mask = g.mask;
        *nanosecondsPerTick = g.nanosecondsPerTick;
        return g.on.load();
    }

    // A submission of two command buffers, which the queue takes or refuses: the pair
    // its timestamps went to, or -1 untimed.
    int32_t rig_cpp_gpu_submit(bool accepted)
    {
        const VkCommandBuffer mine[2] = { Fake<VkCommandBuffer>(0xB000), Fake<VkCommandBuffer>(0xB001) };
        VkSubmitInfo submit{ VK_STRUCTURE_TYPE_SUBMIT_INFO };
        submit.commandBufferCount = 2;
        submit.pCommandBuffers = mine;
        rig::s.queueSubmit = accepted ? VK_SUCCESS : VK_ERROR_DEVICE_LOST;
        vkg::gpu_time::Submit(Fake<VkQueue>(0xA002), submit, VK_NULL_HANDLE);
        if (rig::s.batch.size() != 4) return -1;
        for (uint32_t pair = 0; pair < kPairs; pair++)
            if (rig::s.batch[0] == g.begins[pair] && rig::s.batch[3] == g.ends[pair]) return int32_t(pair);
        return -2;
    }

    struct Collected
    {
        uint64_t collected, nanoseconds, lastEnd;
        uint32_t anyEnd;
        uint64_t seen[64][2];
    };

    // Collect over the queries' values and availability, and each pair's result.
    void rig_cpp_gpu_collect(const uint64_t query[128][2], const int32_t results[64], Collected* out)
    {
        std::memcpy(rig::s.query, query, sizeof rig::s.query);
        for (uint32_t i = 0; i < 64; i++) rig::s.queryResult[i] = VkResult(results[i]);
        rig_report::spent = 0;
        vkg::gpu_time::Collect();
        out->collected = g.collected.load();
        out->nanoseconds = rig_report::spent;
        out->lastEnd = g.lastEnd;
        out->anyEnd = g.anyEnd;
        std::memcpy(out->seen, g.seen, sizeof g.seen);
    }
}
