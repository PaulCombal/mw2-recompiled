#include "install.h"
#include "crypto.h"
#include "xex.h"
#include "spawn.h"
#include "../log.h"

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <string>

#ifdef _WIN32
#include <windows.h>
#endif
#ifdef MW2_USE_SDL
#include <SDL3/SDL.h>
#endif

namespace fs = std::filesystem;

// The executable this build was recompiled from, by its SHA-256, which CMake
// reads from mw2/ at configure time. Any other -- another region, another
// version -- has other code at other addresses, so it is not run.
namespace
{
#ifdef MW2_TITLE_MP
    constexpr const char* kXex = "default_mp.xex";
    constexpr const char* kSha256 = MW2_XEX_SHA256_MP;
#else
    constexpr const char* kXex = "default.xex";
    constexpr const char* kSha256 = MW2_XEX_SHA256_SP;
#endif
    constexpr const char* kGameFolder = "game";
#ifdef _WIN32
    constexpr const char* kLauncher = "mw2-launcher.exe";
#else
    constexpr const char* kLauncher = "mw2-launcher";
#endif

    // Paths as UTF-8, which SDL and the log take, whatever the system's code page.
    std::string Utf8(const fs::path& path)
    {
        const auto text = path.u8string();
        return std::string(text.begin(), text.end());
    }

    fs::path ExecutableFolder()
    {
#ifdef _WIN32
        wchar_t path[MAX_PATH * 4];
        const DWORD length = GetModuleFileNameW(nullptr, path, DWORD(std::size(path)));
        return fs::path(std::wstring(path, length)).parent_path();
#else
        std::error_code ec;
        return fs::read_symlink("/proc/self/exe", ec).parent_path();
#endif
    }

    void ShowError(const std::string& text)
    {
        LOGE("%s", text.c_str());
        std::fprintf(stderr, "%s\n", text.c_str());
#ifdef MW2_USE_SDL
        SDL_ShowSimpleMessageBox(SDL_MESSAGEBOX_ERROR, "Modern Warfare 2", text.c_str(), nullptr);
#endif
    }

    // The launcher beside this executable, left running on its own.
    bool StartLauncher(const std::vector<std::string>& arguments = {})
    {
#ifdef MW2_USE_SDL
        std::error_code ec;
        const fs::path launcher = fs::absolute(kLauncher, ec);
        if (!fs::is_regular_file(launcher, ec)) return false;
        const std::string path = Utf8(launcher);
        std::vector<const char*> args{ path.c_str() };
        for (const std::string& argument : arguments) args.push_back(argument.c_str());
        args.push_back(nullptr);
        SDL_Process* process = spawn::Start(args.data());
        if (!process) { LOGE("cannot start %s: %s", path.c_str(), SDL_GetError()); return false; }
        SDL_DestroyProcess(process);    // forgets it; the launcher keeps running
        return true;
#else
        (void)arguments;
        return false;
#endif
    }

    // The program of each of the title's executables, beside this one.
    const char* ProgramFor(const std::string& xex)
    {
#ifdef _WIN32
        if (xex == "default.xex") return "mw2-sp.exe";
        if (xex == "default_mp.xex") return "mw2-mp.exe";
#else
        if (xex == "default.xex") return "mw2-sp";
        if (xex == "default_mp.xex") return "mw2-mp";
#endif
        return nullptr;
    }

    constexpr const char* kLaunchDataVariable = "MW2_LAUNCH_DATA";
    bool g_playerStart = false;
    std::string g_nextTitle;

    enum class Installed { Yes, No, Unreadable };

    // The title's image from game/, if the executable there is the one this
    // build was made from.
    Installed LoadInstalled(std::vector<uint8_t>& image, std::string& error)
    {
        const fs::path path = fs::path(kGameFolder) / kXex;
        std::vector<uint8_t> file;
        {
            std::ifstream in(path, std::ios::binary);
            file.assign(std::istreambuf_iterator<char>(in), {});
        }
        if (file.empty()) return Installed::No;
        install::crypto::Sha256 hash;
        hash.Update(file.data(), file.size());
        if (hash.FinalHex() != kSha256) return Installed::No;
        if (!install::xex::Image(file, image, error))
        {
            error = Utf8(path) + ": " + error;
            return Installed::Unreadable;
        }
        return Installed::Yes;
    }
}

extern "C" void mw2_install_folder(int argc, void* out, mw2_text_sink sink)
{
    const std::string folder = argc == 1 ? Utf8(ExecutableFolder()) : std::string();
    sink(out, folder.data(), folder.size());
}

extern "C" bool mw2_install_prepare(void* out, mw2_text_sink sink, int* exitCode)
{
    *exitCode = 1;
    g_playerStart = true;
    // Everything a player's copy keeps -- game/, saves/ -- is beside the executable.
    std::error_code ec;
    if (const fs::path folder = ExecutableFolder(); !folder.empty()) fs::current_path(folder, ec);

    std::string error;
    std::vector<uint8_t> image;
    switch (LoadInstalled(image, error))
    {
    case Installed::Yes:
        sink(out, reinterpret_cast<const char*>(image.data()), image.size());
        return true;
    case Installed::Unreadable:
        ShowError(error);
        return false;
    case Installed::No:
        break;
    }
    // Not installed, or installed for another version of this build: both are
    // the launcher's to put right.
    if (StartLauncher()) { *exitCode = 0; return false; }
    ShowError(std::string("The game is not installed here, and ") + kLauncher +
              " is not beside this program to install it.");
    return false;
}

extern "C" bool mw2_install_load_image(const char* utf8, void* out, mw2_text_sink sink)
{
    const fs::path path(reinterpret_cast<const char8_t*>(utf8));
    std::ifstream in(path, std::ios::binary);
    if (!in) { LOGE("cannot open image %s", Utf8(path).c_str()); return false; }
    std::vector<uint8_t> file(std::istreambuf_iterator<char>(in), {});
    if (file.size() >= 4 && std::memcmp(file.data(), "XEX2", 4) == 0)
    {
        std::string error;
        std::vector<uint8_t> image;
        if (!install::xex::Image(file, image, error)) { LOGE("%s: %s", Utf8(path).c_str(), error.c_str()); return false; }
        sink(out, reinterpret_cast<const char*>(image.data()), image.size());
        return true;
    }
    sink(out, reinterpret_cast<const char*>(file.data()), file.size());
    return true;
}

static std::vector<uint8_t>& LaunchDataHeld()
{
    static std::vector<uint8_t> data = [] {
        std::vector<uint8_t> bytes;
        const char* hex = std::getenv(kLaunchDataVariable);
        for (; hex && hex[0] && hex[1]; hex += 2)
        {
            unsigned value = 0;
            if (std::sscanf(hex, "%2x", &value) != 1) return std::vector<uint8_t>{};
            bytes.push_back(uint8_t(value));
        }
        return bytes;
    }();
    return data;
}

extern "C" void mw2_install_launch_data(void* out, mw2_text_sink sink)
{
    const std::vector<uint8_t>& data = LaunchDataHeld();
    sink(out, reinterpret_cast<const char*>(data.data()), data.size());
}

extern "C" void mw2_install_set_launch_data(const uint8_t* data, size_t size)
{
    LaunchDataHeld().assign(data, data + size);
}

extern "C" bool mw2_install_compile_shaders(const char* device, const char* whyText)
{
    const std::string why = whyText;
    // The launcher names the device it compiled for in the game it starts
    // after a pass (launcher/main.cpp).
    const char* compiledFor = std::getenv("MW2_SHADERS_COMPILED_FOR");
    if (compiledFor && !std::strcmp(device, compiledFor))
    {
        ShowError("The shaders the launcher has just compiled do not fit this game: " + why +
                  "\n\nThe game starts anyway, and compiles shaders as it meets them, which stutters. "
                  "Please report this (REPORT A BUG in the launcher).");
        return false;
    }
    LOGI("shaders: %s The launcher compiles them for device %s, then starts the game.", why.c_str(), device);
#ifdef MW2_TITLE_MP
    const char* title = "multiplayer";
#else
    const char* title = "campaign";
#endif
    if (StartLauncher({ "--play", title, "--device", device })) return true;
    ShowError(why + "\n\n" + kLauncher + " is not beside this program to compile them. The game starts anyway, "
              "and compiles shaders as it meets them, which stutters.");
    return false;
}

extern "C" void mw2_install_set_next_title(const char* xex) { g_nextTitle = xex; }

extern "C" void mw2_install_start_next_title()
{
    if (g_nextTitle.empty()) return;
    const char* program = ProgramFor(g_nextTitle);
    if (!program) { LOGE("the title asked for %s, which is not one of its executables", g_nextTitle.c_str()); return; }
    // A development run was given its image and game folder by hand, and
    // nothing says where the other title's are.
    if (!g_playerStart)
    {
        LOGW("the title asked for %s: start that build yourself, a development run does not", g_nextTitle.c_str());
        return;
    }
#ifdef MW2_USE_SDL
    std::error_code ec;
    const fs::path file = fs::absolute(program, ec);
    if (!fs::is_regular_file(file, ec)) { ShowError(std::string(program) + " is not beside this program."); return; }
    std::string hex;
    for (const uint8_t byte : LaunchDataHeld())
    {
        char text[3];
        std::snprintf(text, sizeof(text), "%02x", byte);
        hex += text;
    }
    SDL_Environment* environment = SDL_GetEnvironment();
    if (hex.empty()) SDL_UnsetEnvironmentVariable(environment, kLaunchDataVariable);
    else SDL_SetEnvironmentVariable(environment, kLaunchDataVariable, hex.c_str(), true);
    // It writes on in this run's log, where there is one (main.cpp).
    SDL_SetEnvironmentVariable(environment, "MW2_LOG_APPEND", "1", true);
    const std::string path = Utf8(file);
    const char* const args[] = { path.c_str(), nullptr };
    SDL_Process* process = spawn::Start(args, environment);
    if (!process) { ShowError("cannot start " + path + ": " + SDL_GetError()); return; }
    SDL_DestroyProcess(process);
    LOGI("started %s", path.c_str());
#else
    LOGE("this build cannot start %s", program);
#endif
}
