// What the C++ of a module with a clock runs on in tools/runtime_diff.cpp: the rig's
// time, as the Rust is given it.
#pragma once
#include <chrono>
#include <cstdint>

extern "C" uint64_t rig_nanoseconds;

namespace std::chrono
{
    struct rig_clock
    {
        using rep = int64_t;
        using period = nano;
        using duration = nanoseconds;
        using time_point = chrono::time_point<rig_clock>;
        static constexpr bool is_steady = true;
        static time_point now() { return time_point(duration(int64_t(rig_nanoseconds))); }
    };
}
#define steady_clock rig_clock
