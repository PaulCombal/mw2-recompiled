// Included before command_processor.cpp by tools/gpu_diff.py --rig pm4: its
// std::this_thread::sleep_for calls go to the rig's rig_loop_sleep (pm4.cpp), as the
// Rust's gpu::sleep_for does in a rig build, so the two sides' sleeps are compared.
#pragma once
#include <chrono>
#include <cstdint>
#include <thread>

extern "C" void rig_loop_sleep(uint64_t nanoseconds);
namespace std::this_thread
{
    template <class Rep, class Period>
    void rig_sleep_for(const std::chrono::duration<Rep, Period>& time)
    {
        rig_loop_sleep(uint64_t(std::chrono::duration_cast<std::chrono::nanoseconds>(time).count()));
    }
}
#define sleep_for rig_sleep_for
