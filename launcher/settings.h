#pragma once
// What the player sets in the launcher for the game: kept in
// .env beside the launcher, a line each of NAME=value. The game reads the file as
// it starts (runtime/settings.cpp), however it is started, so a change is
// for the next start. The file can hold any of the game's switches; the
// launcher changes its own and keeps the rest.
#include <optional>
#include <span>
#include <string>

namespace settings
{
    void Load();

    // MW2_FULLSCREEN: the game starts fullscreen (1) or in a window.
    bool Fullscreen();
    void SetFullscreen(bool on);

    // MW2_SCALE: how many times the title's frame the game draws at.
    constexpr int kMaxScale = 3;
    int Scale();

    // The picture's shape and the size the game draws at (ASPECT RATIO and
    // RESOLUTION), each resolution the title's frame times a scale: MW2_RENDER,
    // the frame (runtime/render_size.h), unset for the console's 1280x720, and
    // MW2_SCALE. Changing the shape keeps the row nearest in height.
    struct Resolution
    {
        const char* name;            // 1920x1200
        unsigned width, height;      // the title's frame
        int scale;
    };
    enum class Ratio { Wide, Wider, Standard };   // 16:9, the console's, 16:10 and 4:3
    constexpr int kRatios = 3;
    constexpr Resolution kWideRows[] = {
        { "1280x720", 1280, 720, 1 }, { "1920x1080", 960, 540, 2 }, { "2560x1440", 1280, 720, 2 }, { "3840x2160", 1280, 720, 3 },
    };
    constexpr Resolution kWiderRows[] = {
        { "1280x800", 1280, 800, 1 }, { "1920x1200", 960, 600, 2 }, { "2560x1600", 1280, 800, 2 }, { "3840x2400", 1280, 800, 3 },
    };
    constexpr Resolution kStandardRows[] = {
        { "640x480", 640, 480, 1 },   { "800x600", 800, 600, 1 },     { "1024x768", 1024, 768, 1 },   { "1280x960", 640, 480, 2 },
        { "1600x1200", 800, 600, 2 }, { "1920x1440", 960, 720, 2 },   { "2880x2160", 960, 720, 3 },
    };
    constexpr std::span<const Resolution> Resolutions(Ratio ratio)
    {
        using Rows = std::span<const Resolution>;
        return ratio == Ratio::Wider ? Rows(kWiderRows) : ratio == Ratio::Standard ? Rows(kStandardRows) : Rows(kWideRows);
    }
    // "16:10", and its width over its height.
    const char* RatioName(Ratio ratio);
    float Aspect(Ratio ratio);
    // The kept ones; a frame and scale written by hand that are no row are
    // shown as the row of their shape with their scale, or the first.
    Ratio AspectRatio();
    int ResolutionRow();
    const Resolution& CurrentResolution();
    void SetResolution(Ratio ratio, int row);
    // The next shape, with the row of it nearest in height to the kept one.
    void NextAspectRatio();

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
