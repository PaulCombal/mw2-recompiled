#pragma once
// What the player sets in the launcher for the game: kept in
// .env beside the launcher, a line each of NAME=value. The game reads the file as
// it starts (runtime/settings.cpp), however it is started, so a change is
// for the next start. The file can hold any of the game's switches; the
// launcher changes its own and keeps the rest.
#include <optional>
#include <string>

namespace settings
{
    void Load();

    // MW2_FULLSCREEN: the game starts fullscreen (1) or in a window.
    bool Fullscreen();
    void SetFullscreen(bool on);

    // MW2_SCALE: how many times the console's 1280x720 the game draws at.
    constexpr int kMaxScale = 3;
    int Scale();
    void SetScale(int scale);

    // MW2_FPS_LIMIT: how many frames a second the game draws at most; 60 is
    // the console's own pacing and 0 no limit. HasFpsLimit is false until one
    // is kept, and FpsLimit is 60 then, as the game takes it.
    constexpr int kConsoleFps = 60;
    bool HasFpsLimit();
    int FpsLimit();
    void SetFpsLimit(int limit);

    // MW2_FOV: the game's field of view, as its own setting counts it -- the
    // width of a 4:3 picture, 65 on the console.
    constexpr int kConsoleFov = 65, kWidestFov = 120, kFovStep = 5;
    int Fov();
    void SetFov(int fov);

    // Antialiasing: the samples a pixel of the game's multisampled surfaces
    // has. 2 is the console's own and writes nothing; 4 and 8 are
    // MW2_MSAA=<n>; 0 is MW2_NO_MSAA=1.
    int Msaa();
    void SetMsaa(int samples);

    // MW2_CUSTOM_MAPS: the multiplayer loads fastfiles nobody signed, a map
    // converted from the PC game (runtime/unsigned_fastfiles.cpp).
    bool CustomMaps();
    void SetCustomMaps(bool on);

    // MW2_LAUNCHER_SOUNDS: 0 keeps the launcher's menus silent. Written by hand.
    bool Sounds();

    // MW2_PC_GAME: the PC game's folder, whose pictures and sounds a map
    // converted from the PC takes (maps.h); nullopt when .env has no such
    // line, empty when it says there is none.
    std::optional<std::string> PcGame();
    void SetPcGame(const std::string& folder);
}
