#pragma once
// The frame's Vulkan commands, recorded on a thread of their own.
//
// The ring consumer parses the title's command stream and turns each draw into
// Vulkan calls, and it is what holds the frame rate down: the driver's work in
// those calls was a fifth of its time. What has to stay where the stream is --
// reading guest memory, the register state, choosing targets and pipelines,
// writing the arena -- stays; the calls themselves are queued, each with its
// arguments copied, and a second thread makes them in the order they were
// queued. The submission is queued the same way, and so is everything that has
// to follow it: handing the frame to the window, a RenderDoc boundary.
//
// MW2_RECORD_THREAD=0 makes every call where it is queued, on the consumer, as
// before.
//
// What a queued call names has to outlive it. Everything that destroys a
// Vulkan object the frame may use, or waits for a submission's fence, first
// waits for the recorder to get that far: Drain, or WaitFor a position.
#include <cstdint>
#include <new>
#include <type_traits>
#include <utility>

#include <vulkan/vulkan.h>

namespace vk::record
{
    // At the renderer's start and end. Stop runs what is queued first.
    // `settle`, when given, runs on the recorder before every call that was
    // not queued as Joining: what joining calls held back, it makes.
    void Start(void (*settle)() = nullptr);
    void Stop();
    bool Threaded();

    // The command buffer the calls queued from here on go into.
    void Into(VkCommandBuffer command);

    // Where the queue has got to: the position just past the last call queued.
    // Done says whether the recorder has made every call before a position,
    // WaitFor waits until it has, Drain until it has made them all. Positions
    // are all 0, and everything done, when there is no thread.
    uint64_t Position();
    // Wakes the recorder for whatever is queued, which it otherwise lets
    // gather (recorder.cpp): for when nothing more is coming for a while.
    void Flush();
    bool Done(uint64_t position);
    void WaitFor(uint64_t position);
    void Drain();

    void Report();

    namespace detail
    {
        struct alignas(32) Header
        {
            void (*run)(void* self, VkCommandBuffer command);   // null: skip to the start
            VkCommandBuffer command;
            uint32_t bytes;     // the header's and the call's, to the next header
            uint32_t joins;     // queued as Joining: settle is not run before it
        };
        static_assert(sizeof(Header) == 32);
        extern bool threaded;
        extern VkCommandBuffer into;
        extern void (*settle)();
        // Room for `bytes` at the end of the queue, and making it visible:
        // `now` wakes the recorder for it whatever else is queued.
        void* Reserve(uint32_t bytes);
        void Publish(bool now);
    }

    // `call(VkCommandBuffer)` with the buffer Into last named, on the recorder
    // thread, after every call queued before it. It must copy what it uses: it
    // runs later, and nothing it points at may be changed or freed until then.
    template <class Call> void Command(Call&& call, bool now = false, bool joins = false)
    {
        using T = std::decay_t<Call>;
        if (!detail::threaded)
        {
            if (!joins && detail::settle) detail::settle();
            call(detail::into);
            return;
        }
        static_assert(alignof(T) <= alignof(detail::Header));
        constexpr uint32_t bytes =
            uint32_t((sizeof(detail::Header) + sizeof(T) + sizeof(detail::Header) - 1) &
                     ~(sizeof(detail::Header) - 1));
        void* at = detail::Reserve(bytes);
        auto* header = new (at) detail::Header{
            [](void* self, VkCommandBuffer command) {
                T* made = static_cast<T*>(self);
                (*made)(command);
                made->~T();
            },
            detail::into, bytes, joins ? 1u : 0u };
        new (header + 1) T(std::forward<Call>(call));
        detail::Publish(now);
    }

    // A call that may hold its work back to make it together with the
    // joining calls after it: the renderer's draws, made as one multi-draw
    // while nothing changes between them. Whatever it holds back, `settle`
    // makes before the next call that is not joining, so a held draw is made
    // only once a call after it is: Done and Drain do not wait for it.
    template <class Call> void Joining(Call&& call)
    {
        Command(std::forward<Call>(call), false, true);
    }

    // Work that is not a command but has to come after the ones before it: a
    // submission, the frame handed to the window. It wakes the recorder.
    template <class Call> void Host(Call&& call)
    {
        Command([call = std::forward<Call>(call)](VkCommandBuffer) mutable { call(); }, true);
    }
}
