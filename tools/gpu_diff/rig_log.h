// What each side of a rig printed: the C++ (runtime/log.h) and the Rust (src/log.rs)
// both write their lines to the C library's stderr as it stands at the line, so the rig
// points it at memory around each side's calls and compares the two texts.
//
//   Take(0); <the C++'s calls>; Take(1); <the Rust's>; Stop();  then g_log[0] == g_log[1]
#pragma once
#include <cstdio>
#include <cstdlib>
#include <string>

namespace rig_log
{
    inline std::string g_log[2];
    inline FILE* g_kept = nullptr;
    inline FILE* g_taking = nullptr;
    inline char* g_text = nullptr;
    inline size_t g_size = 0;
    inline int g_side = -1;

    inline void Stop()
    {
        if (g_side < 0) return;
        std::fflush(g_taking);
        stderr = g_kept;
        std::fclose(g_taking);
        g_log[g_side].append(g_text, g_size);
        std::free(g_text);
        g_text = nullptr;
        g_side = -1;
    }

    inline void Take(int side)
    {
        Stop();
        g_kept = stderr;
        g_taking = open_memstream(&g_text, &g_size);
        if (!g_taking) std::abort();
        stderr = g_taking;
        g_side = side;
    }

    // True when both printed the same; the texts are cleared either way unless kept.
    inline bool Same()
    {
        Stop();
        return g_log[0] == g_log[1];
    }

    inline void Clear()
    {
        g_log[0].clear();
        g_log[1].clear();
    }
}
