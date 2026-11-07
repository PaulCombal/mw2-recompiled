// The C++ command trace as it is (runtime/gpu/vulkan/trace.cpp), included whole for its
// file statics: tools/gpu_diff/recorder.cpp puts them back to a process's start between
// its cases, as the Rust's trace::rig::reset does.
#include "gpu/vulkan/trace.cpp"

#include <new>

void rig_cpp_trace_reset()
{
    {
        std::lock_guard lock(s.lock);
        if (s.file) std::fclose(s.file);
        s.file = nullptr;
    }
    s.~State();
    new (&s) State();
    vk::trace::detail::on = false;
    vk::trace::detail::deterministic = false;
    g_forced = Forced{};
    g_counts = ForcedCounts{};
    t_recorder = false;
}
