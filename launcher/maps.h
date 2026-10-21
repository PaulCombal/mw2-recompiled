#pragma once
// Custom maps: maps converted from the PC game for the 360's multiplayer.
//
// A map is three files, <name>.ff, <name>_load.ff and <name>.arena (what the
// menus show of it, runtime/install/arena.h), and a fourth when it has a
// picture in the game's menus, <name>_preview.ff. A map made for the PC game
// is converted when it is added, its picture made of its loading screen. The ones in the game folder are
// in the game: the multiplayer lists them in its BONUS MAPS menu, which has
// room for kInGame (runtime/custom_map_list.cpp). The others wait in maps/
// beside the launcher, which also keeps each map's picture, <name>.bmp, made
// from the PC map's .iwd when it was added.
#include <cstddef>
#include <filesystem>
#include <string>
#include <vector>

namespace setup
{
    struct Progress;
}

namespace maps
{
    constexpr size_t kInGame = 8;

    struct Map
    {
        std::string name;           // the files' name, mp_toujane_v2
        std::string longname;       // as the arena gives it, or the name
        std::string description;
        bool inGame = false;
        std::filesystem::path picture;   // empty when there is none
    };

    // Every installed map, the game's first, each in the order of its name.
    std::vector<Map> List();
    size_t InGame(const std::vector<Map>& maps);

    // Adds the maps among `files`: each .arena with its two fastfiles, and an
    // .iwd; a file chosen brings its map's others from its folder (they share
    // the map's name). Fastfiles of the 360 are copied as they are; the PC game's are
    // converted for the 360 first (mapport/port.h), with the .iwd's pictures,
    // which takes a while: `progress` follows it and its cancel stops it;
    // `pcGame` is the PC game's folder (PcGame) or empty. The
    // .iwd also gives the map its picture here. The maps go into the game
    // folder while the game has room, else into maps/; the chosen files stay
    // as they are. `said` is what came of it.
    bool Add(const std::vector<std::filesystem::path>& files, std::string& said, setup::Progress* progress = nullptr,
             const std::filesystem::path& pcGame = {});
    // Whether adding `files` (and their maps' other files) converts a map made
    // for the PC game.
    bool Converts(const std::vector<std::filesystem::path>& files);

    // Puts a map in the game or takes it out; the game has room for kInGame.
    bool SetInGame(const Map& map, bool on, std::string& said);

    // Deletes the launcher's copy, in the game folder or maps/, its picture
    // and the log of its conversion.
    bool Remove(const Map& map, std::string& said);

    // The PC game's folder, whose .iwd files hold the pictures and sounds the
    // PC game's own maps share: a map made for the PC may use them without
    // carrying them, and its conversion takes them from there. It is
    // MW2_PC_GAME (from the environment, else .env); when .env has no such
    // line, Steam's copy, searched for once a run and kept there. Empty when
    // there is none (an empty MW2_PC_GAME= line says so for good). For the
    // launcher's own thread: it may write .env.
    std::filesystem::path PcGame();
    // Steam's copy of the PC game: the first of Steam's installations and
    // libraries that has it installed, or empty.
    std::filesystem::path FindPcGame();
    // Keeps `folder` as the PC game's, when it has the game's .iwd files.
    bool SetPcGame(const std::filesystem::path& folder, std::string& said);
}
