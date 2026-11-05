#pragma once
// How a player starts the game: with no arguments. The executable works from
// its own folder, where the launcher (launcher/) installed the game's files
// into game/. With no install there, or another version's, it starts the
// launcher instead.
//
// A run given an image and a game folder on the command line is a
// development run and skips all of this.
#include <cstdint>
#include <filesystem>
#include <string>
#include <vector>
#include "../text_sink.h"

extern "C"
{
    void mw2_install_folder(int argc, void* folder, mw2_text_sink sink);
    bool mw2_install_prepare(void* image, mw2_text_sink sink, int* exitCode);
    bool mw2_install_load_image(const char* path, void* image, mw2_text_sink sink);
    bool mw2_install_compile_shaders(const char* device, const char* why);
    void mw2_install_launch_data(void* data, mw2_text_sink sink);
    void mw2_install_set_launch_data(const uint8_t* data, size_t size);
    void mw2_install_set_next_title(const char* xex);
    void mw2_install_start_next_title();
}

namespace install
{
    struct Launch
    {
        std::vector<uint8_t> image;         // the title's memory image
        std::filesystem::path gameRoot;
    };

    // True when the command line asks for a player's start.
    inline bool PlayerStart(int argc, char**) { return argc == 1; }

    // The folder this start runs in: the executable's for a player's start,
    // which Prepare moves to, and the current directory (empty) otherwise.
    inline std::filesystem::path Folder(int argc, char**)
    {
        std::string folder;
        mw2_install_folder(argc, &folder, mw2_assign_text);
        return std::filesystem::path(reinterpret_cast<const char8_t*>(folder.c_str()));
    }

    // Loads the installed title. Returns false when the run should end, with
    // `exitCode` saying how: the launcher was started, or a failure was shown.
    inline bool Prepare(int, char**, Launch& launch, int& exitCode)
    {
        if (!mw2_install_prepare(&launch.image, mw2_assign_bytes, &exitCode)) return false;
        launch.gameRoot = "game";
        return true;
    }

    // A development run's image: a flat PE, or an XEX decrypted on the way in.
    inline bool LoadImageFile(const std::filesystem::path& path, std::vector<uint8_t>& image)
    {
        const std::u8string text = path.u8string();
        return mw2_install_load_image(reinterpret_cast<const char*>(text.c_str()), &image, mw2_assign_bytes);
    }

    // Starts the launcher to compile every shader of the game for `device`
    // (vk::pipeline::DeviceId) and then start this title, when the shaders
    // the launcher compiled do not fit this run, for the reason `why`. False
    // when that is no help: the launcher is not there, or it has just
    // compiled for this device and they still do not fit -- the run goes on
    // then, after saying so.
    inline bool CompileShaders(const std::string& device, const std::string& why)
    {
        return mw2_install_compile_shaders(device.c_str(), why.c_str());
    }

    // The data one executable of the title leaves for the next (the console's
    // XamLoaderSetLaunchData): the campaign and the multiplayer are two
    // programs here as there, and it crosses from one to the other in the
    // environment. Empty when nothing left any.
    inline std::vector<uint8_t> LaunchData()
    {
        std::vector<uint8_t> data;
        mw2_install_launch_data(&data, mw2_assign_bytes);
        return data;
    }
    inline void SetLaunchData(const uint8_t* data, size_t size) { mw2_install_set_launch_data(data, size); }

    // What the title asked to be started in its place, by the executable's
    // name on the disc ("default_mp.xex"), and the start itself, made once
    // this run has let go of the window and the sound.
    inline void SetNextTitle(const std::string& xex) { mw2_install_set_next_title(xex.c_str()); }
    inline void StartNextTitle() { mw2_install_start_next_title(); }
}
