// runtime/gpu/gpu.cpp as it is, compiled into the interrupts rig: its file-static state,
// RaiseInterrupt, and the consumer's and the vblank's loops reached from here. What it
// calls of the renderer, the presenter and the capture is the rig's (interrupts.cpp) or
// never reached (the rig links with those unresolved); what it calls of the kernel and
// the guest's memory is the Rust runtime's, as in the game.
//
// The loops run on the rig's clock: every header gpu.cpp includes is included first, as
// it is, and then the names of the clock, the sleep, the time stamp bundle and the
// display's blank period are turned to the rig's for gpu.cpp's own text alone.
#include <ppc_recomp_shared.h>
#include "title.h"
#include "pacing_trace.h"
#include "stutters.h"
#include "report.h"
#include "gpu/memory_watch.h"
#include "gpu/internal.h"
#include "gpu/gpu.h"
#include "crash.h"
#include "diagnostics.h"
#include "guest.h"
#include "image_move.h"
#include "log.h"
#include "kernel/kernel.h"
#include "gpu/vulkan/pipeline.h"
#include "gpu/vulkan/presenter.h"
#include "gpu/vulkan/renderer.h"
#include "gpu/vulkan/texture_cache.h"
#include "gpu/stream_capture.h"

#include <atomic>
#include <chrono>
#include <mutex>
#include <thread>

extern "C"
{
    uint64_t cpp_loop_now();
    void cpp_loop_sleep(uint64_t nanoseconds);
    void cpp_loop_bundle();
    uint64_t cpp_loop_blank_period();
    void cpp_loop_deliver();
    void cpp_loop_execute(uint32_t base, uint32_t readIndex, uint32_t mask, uint32_t dwords);
    void cpp_loop_batch_begin(uint32_t base, uint32_t readIndex, uint32_t mask, uint32_t dwords);
    void cpp_loop_batch_end();
    void cpp_loop_idle();
}

// What the consumer calls of the command processor, the renderer and the capture: the
// rig's record (interrupts.cpp), as the Rust's calls through runtime/rust_calls.cpp are.
void gpu::detail::DeliverOcclusionQueries() { cpp_loop_deliver(); }
void gpu::detail::ExecuteRing(uint32_t base, uint32_t readIndex, uint32_t mask, uint32_t dwords)
{
    cpp_loop_execute(base, readIndex, mask, dwords);
}
void gpu::stream::BatchBegin(uint32_t base, uint32_t readIndex, uint32_t mask, uint32_t dwords)
{
    cpp_loop_batch_begin(base, readIndex, mask, dwords);
}
void gpu::stream::BatchEnd() { cpp_loop_batch_end(); }
void vk::renderer::Idle() { cpp_loop_idle(); }

namespace std::chrono
{
    struct rig_clock
    {
        using duration = nanoseconds;
        using rep = duration::rep;
        using period = duration::period;
        using time_point = std::chrono::time_point<rig_clock>;
        static constexpr bool is_steady = true;
        static time_point now() { return time_point(duration(cpp_loop_now())); }
    };
}
namespace std::this_thread
{
    template <class Rep, class Period> void rig_sleep_for(const std::chrono::duration<Rep, Period>& time)
    {
        cpp_loop_sleep(uint64_t(std::chrono::duration_cast<std::chrono::nanoseconds>(time).count()));
    }
}
namespace kernel
{
    inline void RigUpdateTimeStampBundle() { cpp_loop_bundle(); }
}
namespace vk
{
    inline uint64_t RigGuestBlankPeriod() { return cpp_loop_blank_period(); }
}

#define steady_clock rig_clock
#define sleep_for rig_sleep_for
#define UpdateTimeStampBundle RigUpdateTimeStampBundle
#define GuestBlankPeriod RigGuestBlankPeriod
#include "gpu/gpu.cpp"
#undef steady_clock
#undef sleep_for
#undef UpdateTimeStampBundle
#undef GuestBlankPeriod

namespace
{
    using Raise = void (*)(uint32_t pcr, uint32_t mask, bool blank);

    // As the consumer or the vblank raises: the thread's PCR, then a CPU mask, or the
    // vblank's status word and interrupt, as VBlankWorker has them at a blank. The
    // interrupts part raises from two threads at once with this; the vblank part runs
    // VBlankWorker itself.
    void CppRaise(uint32_t pcr, uint32_t mask, bool blank)
    {
        t_guestPcr = pcr;
        if (!blank)
        {
            gpu::detail::RaiseCommandProcessorInterrupt(mask);
            return;
        }
        g.vblanks++;
        *GuestPtr<be32>(kDisplayStatus) = kDisplayStatusVBlank;
        std::lock_guard<std::mutex> turn(g.guestCall);
        RaiseInterrupt(kVBlankSource);
    }
}

extern "C" void cpp_rig_set_interrupt_callback(uint32_t callback, uint32_t context)
{
    gpu::SetInterruptCallback(callback, context);
}

// `body(context, raise)` on a thread made as gpu.cpp makes the consumer: a std::thread.
extern "C" void cpp_rig_on_consumer_thread(void (*body)(void*, Raise), void* context)
{
    std::thread thread(body, context, CppRaise);
    thread.join();
}

// The loops, as Initialise starts them, until cpp_rig_stop_loop.
extern "C" void cpp_rig_worker(uint32_t pcr)
{
    g.gpuPcr = pcr;
    g.running = true;
    Worker();
}

extern "C" void cpp_rig_vblank_worker(uint32_t pcr)
{
    g.vblankPcr = pcr;
    g.running = true;
    VBlankWorker();
}

extern "C" void cpp_rig_stop_loop() { g.running = false; }

extern "C" void cpp_rig_forget_ring()
{
    g.ring = Ring{};
    g.rptrWriteBack = 0;
}

extern "C" void cpp_rig_set_ring_buffer(uint32_t basePhysical, uint32_t sizeLog2) { gpu::SetRingBuffer(basePhysical, sizeLog2); }

extern "C" void cpp_rig_set_read_pointer_write_back(uint32_t addressPhysical, uint32_t blockSizeLog2)
{
    gpu::SetReadPointerWriteBack(addressPhysical, blockSizeLog2);
}

extern "C" uint32_t cpp_rig_write_back() { return g.rptrWriteBack; }

extern "C" void cpp_rig_loop_counts(uint64_t* vblanks, uint64_t* busyNanoseconds)
{
    *vblanks = g.vblanks.load();
    *busyNanoseconds = g.consumerBusyNanoseconds.load();
}
