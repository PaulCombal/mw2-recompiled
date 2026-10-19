#pragma once
// Every shader of the game compiled before the game first runs.
//
// On the console a shader is ready the moment the title loads it. Here each has
// to be translated and compiled for the host's GPU, and one the game meets
// first in play is compiled there and then -- on a cold driver cache, tens of
// milliseconds of a frame each, hundreds in a level. The shaders are all in
// the game's files, so the launcher reads them out (zone/), builds each the
// way the game would (runtime/gpu/vulkan/shader_build.h) into the pipeline
// cache the game starts with, and the game finds every one there.
//
// It is not optional: the launcher runs it after an install and whenever
// the marker it leaves (runtime/gpu/vulkan/shader_marker.h) no longer fits
// -- another version, driver, graphics card, resolution or game file -- before
// it starts the game, and a game started on its own starts the launcher for
// it.
#include <atomic>
#include <cstdint>
#include <map>
#include <string>

namespace precompile
{
    struct Progress
    {
        std::atomic<bool> cancel{ false };
        std::atomic<bool> checked{ false };   // the marker has been looked at: reading or compiling now
        std::string why;                      // what no longer fits; set before `checked`
        std::atomic<uint32_t> files{ 0 }, filesRead{ 0 };
        std::atomic<uint32_t> shaders{ 0 }, compiled{ 0 };
    };

    struct Result
    {
        bool ok = false;
        bool cancelled = false;
        bool current = false;         // nothing to do: the marker fits
        std::string error;            // why not, when not
        std::string why;              // why it ran: what no longer fit
        std::string device;           // the GPU it compiled for, by name
        std::string deviceId;         // and by vk::pipeline::DeviceId
        bool noLibraries = false;     // the device compiles shaders only with a draw's state: none could be
        uint32_t shaders = 0;         // distinct shaders in the game
        uint32_t untranslated = 0;    // the translator cannot make (the game cannot draw them either)
        std::map<std::string, uint32_t> reasons;   // why, and how many
        uint32_t refused = 0;         // translated, and the driver would not compile
        uint32_t fromCache = 0;       // already in the cache of an earlier pass for the same build and device
        double readSeconds = 0, compileSeconds = 0;
        uint64_t cacheBytes = 0;
    };

    // Brings the compiled shaders up to date: checks the marker and, unless
    // it fits (or `always`), reads game/ and builds every shader in it, on
    // every core. `device` (vk::pipeline::DeviceId) is the one the game
    // draws on when it named one; else the one the marker names, if the
    // machine still has it; else the one the game's window would pick.
    // Blocks until done. Run from the folder the launcher is in.
    Result Run(const std::string& device, bool always, Progress& progress);
}
