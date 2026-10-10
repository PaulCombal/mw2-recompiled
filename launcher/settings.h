#pragma once
// What the player sets in the launcher for the game: kept in
// .env beside the launcher, a line each of NAME=value. The game reads the file as
// it starts (runtime/settings.cpp), however it is started, so a change is
// for the next start. The file can hold any of the game's switches; the
// launcher changes its own and keeps the rest.
namespace settings
{
    void Load();

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

    // MW2_LAUNCHER_SOUNDS: 0 keeps the launcher's menus silent. Written by hand.
    bool Sounds();
}
