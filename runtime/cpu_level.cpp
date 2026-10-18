// The processor the game is built for, checked before anything else runs.
//
// The game is compiled for x86-64-v2 (MW2_X86_LEVEL in CMakeLists.txt), which
// adds SSSE3, SSE4.1, SSE4.2 and POPCNT to the plain x86-64 baseline. The
// recompiled code needs SSSE3 most: the 360's vector unit reads memory
// big-endian, so nearly every vector load, store and permute is a byte shuffle,
// which without pshufb SIMDe makes into sixteen single-byte moves.
//
// A processor without them would stop on the first such instruction, perhaps
// in a static constructor, with nothing said. This file alone is compiled for
// the baseline, and its constructor runs before every unprioritised one.
#include <cstdio>
#include <cstdlib>
#ifdef MW2_USE_SDL
#include <SDL3/SDL.h>
#endif

namespace
{
    __attribute__((constructor(101))) void CheckProcessor()
    {
        __builtin_cpu_init();
        if (__builtin_cpu_supports("ssse3") && __builtin_cpu_supports("sse4.1") &&
            __builtin_cpu_supports("sse4.2") && __builtin_cpu_supports("popcnt"))
            return;
        const char* text =
            "This processor is too old for the game: it needs SSE4.2, which Intel Core "
            "processors have had since 2008 and AMD processors since 2011.";
        std::fprintf(stderr, "%s\n", text);
#ifdef MW2_USE_SDL
        SDL_ShowSimpleMessageBox(SDL_MESSAGEBOX_ERROR, "Modern Warfare 2", text, nullptr);
#endif
        std::_Exit(1);
    }
}
