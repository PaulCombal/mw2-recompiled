#pragma once
// Converting a map made for the PC game (IW4x, or the 2009 game) into one the
// 360 game loads: its .ff (and the _load.ff, .iwd and .arena next to it),
// with what it needs from the console's own files.
#include "porter.h"

#include <exception>
#include <filesystem>
#include <optional>
#include <string>
#include <utility>
#include <vector>

namespace mapport
{
    // A PC map's files: the map, and what came with it.
    struct PcMap
    {
        std::filesystem::path ff;                          // <map>.ff
        std::optional<std::filesystem::path> load;         // <map>_load.ff, its loading screen
        std::optional<std::filesystem::path> iwd;          // <map>.iwd, its pictures
        std::optional<std::filesystem::path> arena;        // <map>.arena, its names and teams
        // The map's files beside `ff`, named after it.
        static PcMap Beside(const std::filesystem::path& ff);
    };

    // Stops a conversion: thrown by the log function given to PortMap.
    // (Not an Error: no handler of a failing step can take it for one.)
    struct Cancelled : std::exception
    {
        const char* what() const noexcept override { return "the conversion was stopped"; }
    };

    struct Ported
    {
        std::vector<std::filesystem::path> written;
        // What the conversion warns of (pictures the .iwd lacks, shader sets
        // taken in place of others, ...), also in the log as notes.
        std::vector<std::string> warnings;
    };

    struct PortOptions
    {
        // The teams; else the map's .arena's, or the game's defaults.
        std::optional<Teams> teams;
        // The PC game's .iwd files (GameIwds), for the pictures and sounds a
        // map takes from the game's own files rather than carrying them.
        std::vector<std::filesystem::path> gameIwds;
    };

    // Converts a PC map into `outDir` (<map>.ff and, when it has one,
    // <map>_load.ff and <map>_preview.ff, the menus' picture), taking what it needs from the stock 360 files given
    // (the game's: code_post_gfx_mp.ff, common_mp.ff and the stock maps and
    // their _load.ff). Throws PortError (or Error) when the map can't be
    // converted.
    Ported PortMap(const PcMap& map, const std::filesystem::path& outDir, const std::vector<std::filesystem::path>& stock,
                   const Log& log, const PortOptions& options = {});

    // The .iwd files in a PC game's folder and its subfolders (main/, IW4x's
    // iw4x/), in the order of their paths; usermaps/ and mods/ are left out,
    // not being the game's own.
    std::vector<std::filesystem::path> GameIwds(const std::filesystem::path& folder);
}
