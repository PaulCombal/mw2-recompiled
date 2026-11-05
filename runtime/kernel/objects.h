#pragma once
#include <cstdint>
#include <memory>

// Kernel objects, as the C++ build's kernel sees them; the Rust build's are
// src/runtime/kernel/objects.rs's.
//
// The handle table and the objects the kernel makes itself -- events, semaphores,
// mutants, timers, threads -- are dispatcher.h's. What the rest of the kernel keeps
// in the table is its own Object -- a file, a notification listener, a session, an
// enumerator -- which a wait finds always signalled. The table holds a shared
// pointer to it and frees that, through the function handed over with it.
extern "C"
{
    uint32_t mw2_kernel_insert_object(void* object, void (*release)(void* object));
    bool mw2_kernel_lookup_object(uint32_t handle, void (*copy)(void* to, void* object), void* to);
    bool mw2_kernel_set_event(uint32_t handle);
}

namespace kernel
{
    // The pseudo-handles the kernel defines for "the caller".
    inline constexpr uint32_t kCurrentThreadHandle  = 0xFFFFFFFFu;
    inline constexpr uint32_t kCurrentProcessHandle = 0xFFFFFFFEu;

    struct Object
    {
        virtual ~Object() = default;
        virtual const char* TypeName() const { return "object"; }
        uint32_t handle = 0;        // the C++ build's own table sets these two
        uint32_t guestHeader = 0;   // the dispatcher header, once it has one
    };

    inline uint32_t InsertHandle(std::shared_ptr<Object> object)
    {
        return mw2_kernel_insert_object(new std::shared_ptr<Object>(std::move(object)),
                                        [](void* kept) { delete static_cast<std::shared_ptr<Object>*>(kept); });
    }

    // Null when the handle names none of the C++'s objects.
    inline std::shared_ptr<Object> LookupHandle(uint32_t handle)
    {
        std::shared_ptr<Object> found;
        mw2_kernel_lookup_object(handle,
                                 [](void* to, void* kept) {
                                     *static_cast<std::shared_ptr<Object>*>(to) =
                                         *static_cast<std::shared_ptr<Object>*>(kept);
                                 },
                                 &found);
        return found;
    }

    template <typename T>
    std::shared_ptr<T> LookupHandleAs(uint32_t handle)
    {
        return std::dynamic_pointer_cast<T>(LookupHandle(handle));
    }

    // Signals the event a handle names, as NtSetEvent does; false when it names none.
    inline bool SetEvent(uint32_t handle) { return mw2_kernel_set_event(handle); }
}
