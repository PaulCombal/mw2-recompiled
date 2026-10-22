// The launcher: what a player opens. It installs the game from their disc,
// starts the campaign and the multiplayer, and is where the tools that work on
// an install go (maps, the profile, updates of this program).
//
//     mw2-launcher                                  the window
//     mw2-launcher --install [<disc>] [--update <package or folder>]
//                                                   the install, in a terminal
//     mw2-launcher --upgrade                        a newer release, in a terminal
//     mw2-launcher --compile-shaders                every shader of the game, in a terminal
//     mw2-launcher --add-map <files>                a custom map (launcher/maps.h), in a terminal
//     mw2-launcher --play campaign|multiplayer [--device <id>]
//                                                   the window, which compiles the shaders
//                                                   if they need it and starts that game
//
// With no <disc>, --install brings the install already in game/ up to date.
// The game starts the launcher with --play when the shaders compiled for it
// no longer fit (runtime/install/install.h), naming the device it draws on.
#include "disc.h"
#include "precompile.h"
#include "fonts.h"
#include "profile.h"
#include "report.h"
#include "settings.h"
#include "sound.h"
#include "setup.h"
#include "ui.h"
#include "update.h"
#include "maps.h"
#include "single.h"
#include "../runtime/install/spawn.h"

#ifdef _WIN32
#include <windows.h>
#include <shellapi.h>
#include <tlhelp32.h>
#endif
#include <SDL3/SDL.h>
#include <imgui.h>
#include <imgui_impl_sdl3.h>
#include <imgui_impl_sdlrenderer3.h>

#include <algorithm>
#include <cmath>
#include <cctype>
#include <atomic>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <functional>
#include <iterator>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <tuple>
#include <vector>

namespace fs = std::filesystem;
using install::FromUtf8;
using install::Utf8;

namespace
{
#ifdef _WIN32
    constexpr const char* kCampaign = "mw2-sp.exe";
    constexpr const char* kMultiplayer = "mw2-mp.exe";
    constexpr const char* kLauncher = "mw2-launcher.exe";
#else
    constexpr const char* kCampaign = "mw2-sp";
    constexpr const char* kMultiplayer = "mw2-mp";
    constexpr const char* kLauncher = "mw2-launcher";
#endif

    // An install running on its own thread, or with `work` given, a look for
    // a newer version of the launcher or its installation, a map converted
    // from the PC game, or the shaders brought up to date.
    struct Job
    {
        enum class Kind { Install, CheckUpdate, InstallUpdate, Compile, AddMap } kind = Kind::Install;
        std::function<setup::Result(setup::Progress&, std::string&)> work;
        fs::path disc, update;
        setup::Progress progress;
        std::atomic<bool> finished{ false };
        setup::Result result = setup::Result::Failed;
        std::string error;
        std::thread thread;
        // Kind::Compile: for which device, and what to do once they are.
        precompile::Progress compile;
        precompile::Result compiled;
        std::string device;
        int then = 0;

        void Start()
        {
            thread = std::thread([this] {
                try
                {
                    if (kind == Kind::Compile)
                    {
                        compiled = precompile::Run(device, false, compile);
                        result = compiled.ok ? setup::Result::Done : compiled.cancelled ? setup::Result::Cancelled : setup::Result::Failed;
                        error = compiled.error;
                    }
                    else result = work ? work(progress, error) : setup::Run(disc, update, progress, error);
                }
                catch (const std::exception& e)   // (what a step didn't foresee: the launcher stays up)
                {
                    result = setup::Result::Failed;
                    error = e.what();
                }
                finished = true;
            });
        }
        void Cancel()
        {
            progress.cancel = true;
            compile.cancel = true;
        }
        ~Job() { if (thread.joinable()) thread.join(); }
    };

    std::string Amount(uint64_t done, uint64_t total)
    {
        char text[64];
        std::snprintf(text, sizeof(text), "%.1f of %.1f GB", done / 1e9, total / 1e9);
        return total ? text : "";
    }

    int InstallInTerminal(const fs::path& disc, const fs::path& update)
    {
        Job job;
        job.disc = disc;
        job.update = update;
        job.Start();
        std::string shown;
        while (!job.finished)
        {
            std::this_thread::sleep_for(std::chrono::milliseconds(250));
            std::lock_guard lock(job.progress.lock);
            char line[200];
            std::snprintf(line, sizeof(line), "\r  %d/%d  %-28s %-18s %-28s", job.progress.step, job.progress.steps,
                          job.progress.title.c_str(), Amount(job.progress.done, job.progress.total).c_str(),
                          job.progress.detail.c_str());
            if (line != shown) { std::fputs(line, stdout); std::fflush(stdout); shown = line; }
        }
        job.thread.join();
        if (!shown.empty()) std::fputs("\n", stdout);
        if (job.result != setup::Result::Done)
        {
            std::fprintf(stderr, "Not installed: %s\n", job.error.c_str());
            if (job.result == setup::Result::NoUpdate)
                std::fprintf(stderr, "Download it from\n  %s\nand give it with --update <file>.\n", setup::UpdateUrl());
            return 1;
        }
        std::printf("Installed into %s.\n", Utf8(fs::absolute(setup::GameFolder())).c_str());
        return 0;
    }

    // Adds a map from a terminal: one made for the PC game is converted, its
    // progress shown as it goes.
    int AddMapInTerminal(const std::vector<fs::path>& files)
    {
        Job job;
        job.kind = Job::Kind::AddMap;
        job.work = [&files, game = maps::PcGame()](setup::Progress& progress, std::string& said) {
            return maps::Add(files, said, &progress, game) ? setup::Result::Done : setup::Result::Failed;
        };
        job.Start();
        std::string shown;
        while (!job.finished)
        {
            std::this_thread::sleep_for(std::chrono::milliseconds(250));
            std::lock_guard lock(job.progress.lock);
            if (!job.progress.steps) continue;
            char line[240];
            std::snprintf(line, sizeof(line), "  %d/%d  %-30s %s", job.progress.step, job.progress.steps,
                          job.progress.title.c_str(), job.progress.detail.c_str());
            if (line != shown) { std::printf("%s\n", line); std::fflush(stdout); shown = line; }
        }
        job.thread.join();
        std::fprintf(job.result == setup::Result::Done ? stdout : stderr, "%s\n", job.error.c_str());
        return job.result == setup::Result::Done ? 0 : 1;
    }

    // Looks for a newer version and installs it, from a terminal.
    int UpgradeInTerminal()
    {
        if (!*update::Current()) { std::fprintf(stderr, "This build was not made as a release, so there is no version to compare.\n"); return 1; }
        update::Release release;
        std::string error;
        switch (update::Look(release, error))
        {
        case update::Check::UpToDate:
            std::printf("This is the newest version, %s.\n", update::Current());
            return 0;
        case update::Check::Failed:
            std::fprintf(stderr, "%s\n", error.c_str());
            return 1;
        case update::Check::Newer:
            break;
        }
        std::printf("Version %s is out; this is %s. Installing it...\n", release.version.c_str(), update::Current());
        setup::Progress progress;
        if (update::Install(release, progress, error) != setup::Result::Done) { std::fprintf(stderr, "Not updated: %s\n", error.c_str()); return 1; }
        std::printf("Updated to %s.\n", release.version.c_str());
        return 0;
    }

    // Compiles every shader of the game, from a terminal.
    int CompileInTerminal()
    {
        precompile::Progress progress;
        precompile::Result result;
        std::atomic<bool> done{ false };
        std::thread pass([&] {
            result = precompile::Run("", true, progress);
            done = true;
        });
        uint32_t shown = ~0u;
        while (!done)
        {
            std::this_thread::sleep_for(std::chrono::milliseconds(200));
            const uint32_t total = progress.shaders;
            const uint32_t now = total ? 100 * progress.compiled / total : 0;
            if (total && now != shown) std::printf("compiling shaders: %u%%\n", shown = now);
        }
        pass.join();
        if (!result.ok)
        {
            std::fprintf(stderr, "Shaders not compiled: %s\n", result.error.c_str());
            return 1;
        }
        if (result.noLibraries)
        {
            std::printf("%s compiles shaders only with the state of a draw, so the game compiles them as it meets"
                        " them\n", result.device.c_str());
            return 0;
        }
        std::printf("%u shaders compiled for %s in %.1f s (reading the game's files %.1f s), %u of them already in"
                    " the cache; %u the translator cannot make, %u the driver refused; a %.0f MB cache\n",
                    result.shaders, result.device.c_str(), result.compileSeconds, result.readSeconds,
                    result.fromCache, result.untranslated, result.refused, double(result.cacheBytes) / 1e6);
        for (const auto& [reason, count] : result.reasons) std::printf("  %u: %s\n", count, reason.c_str());
        return 0;
    }

    // The launcher an update starts is told so: the old one is still there
    // for a moment, and holds the window (single.h).
    constexpr const char* kTakeOver = "MW2_LAUNCHER_TAKES_OVER";

    // Starts a program beside the launcher and leaves it running. A game is
    // told the device its shaders were compiled for (`shadersFor`), so that it
    // does not start the launcher again for them (runtime/install/install.h).
    bool Start(const char* program, const std::string& shadersFor = "")
    {
        std::error_code ec;
        const std::string path = Utf8(fs::absolute(program, ec));
        const char* const args[] = { path.c_str(), nullptr };
        SDL_Environment* environment = SDL_CreateEnvironment(true);
        if (!shadersFor.empty()) SDL_SetEnvironmentVariable(environment, "MW2_SHADERS_COMPILED_FOR", shadersFor.c_str(), true);
        if (!std::strcmp(program, kLauncher)) SDL_SetEnvironmentVariable(environment, kTakeOver, "1", true);
        SDL_Process* process = spawn::Start(args, environment);
        SDL_DestroyEnvironment(environment);
        if (!process) return false;
        SDL_DestroyProcess(process);
        return true;
    }

    // Whether the campaign or the multiplayer is running: the profile is
    // theirs to write while they are, and they write it when they end.
    bool GameRunning()
    {
#ifdef _WIN32
        bool running = false;
        const HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if (snapshot == INVALID_HANDLE_VALUE) return false;
        PROCESSENTRY32W process{};
        process.dwSize = sizeof(process);
        for (BOOL more = Process32FirstW(snapshot, &process); more && !running; more = Process32NextW(snapshot, &process))
            running = !_wcsicmp(process.szExeFile, L"mw2-sp.exe") || !_wcsicmp(process.szExeFile, L"mw2-mp.exe");
        CloseHandle(snapshot);
        return running;
#else
        // The two programs beside the launcher, by the file each process runs.
        std::error_code ec;
        const fs::path campaign = fs::weakly_canonical(kCampaign, ec), multiplayer = fs::weakly_canonical(kMultiplayer, ec);
        for (fs::directory_iterator it("/proc", ec), end; !ec && it != end; it.increment(ec))
        {
            const std::string name = it->path().filename().string();
            if (name.empty() || name.find_first_not_of("0123456789") != std::string::npos) continue;
            std::error_code ignored;
            const fs::path program = fs::read_symlink(it->path() / "exe", ignored);
            if (!ignored && (program == campaign || program == multiplayer)) return true;
        }
        return false;
#endif
    }

    std::string Upper(std::string text)
    {
        for (char& c : text) c = char(std::toupper(static_cast<unsigned char>(c)));
        return text;
    }

    // The pictures the pane shows, loaded once each: a file made again (a map
    // added again) has another time and is loaded again.
    struct Pictures
    {
        struct Loaded
        {
            fs::file_time_type time;
            SDL_Texture* texture = nullptr;
        };
        std::map<std::string, Loaded> loaded;

        SDL_Texture* Get(SDL_Renderer* renderer, const std::string& path)
        {
            if (path.empty()) return nullptr;
            std::error_code ec;
            const fs::file_time_type time = fs::last_write_time(FromUtf8(path), ec);
            if (ec) return nullptr;
            Loaded& entry = loaded[path];
            if (entry.texture && entry.time == time) return entry.texture;
            if (entry.texture) SDL_DestroyTexture(entry.texture);
            entry = { time, nullptr };
            if (SDL_Surface* surface = SDL_LoadBMP(path.c_str()))
            {
                entry.texture = SDL_CreateTextureFromSurface(renderer, surface);
                SDL_DestroySurface(surface);
            }
            return entry.texture;
        }
        ~Pictures() { for (auto& [path, entry] : loaded) if (entry.texture) SDL_DestroyTexture(entry.texture); }
    };

    // What an entry does.
    enum class Action
    {
        None, PlayCampaign, PlayMultiplayer, Install, ChooseUpdate, Cancel, Quit,
        // The profile screen and what it does.
        Profile, Back, NextPlayer, Rename, PlayAs, Rank, Prestige, UnlockEverything, UnlockMissions, AllStars,
        CheckUpdate, InstallUpdate,
        // The graphics screen, and the next of what each of its entries sets.
        Graphics, Fullscreen, AspectRatio, Resolution, Antialiasing, FpsLimit, Fov,
        // The maps screen; CUSTOM MAPS turned on asks first, on a screen of its own.
        Maps, CustomMaps, CustomMapsOn, CustomMapsCancel, PcGame,
        // and its maps: one shown at a time, put in or out of the game, removed after a question.
        AddMap, NextMap, MapInGame, RemoveMap, RemoveMapYes, RemoveMapNo,
        // The bug report screen: a run of either game with its log kept.
        Report, ReportCampaign, ReportMultiplayer,
        // The question a start asks when a recorded run was never reported.
        SendPending, DiscardPending,
    };

    // More than any screen has, so that a screen's list never moves while it is filled.
    constexpr size_t kMostEntries = 32;

    struct Entry
    {
        ui::Entry shown;
        Action action = Action::None;
        std::string heading, text;      // the pane, while the entry is the chosen one
        std::string picture;            // and a picture above its text, a .bmp
    };

    // The file dialog answers on a thread of its own choosing.
    struct Picked
    {
        std::mutex lock;
        bool answered = false, failed = false;
        Action forAction = Action::None;
        std::string path;
        std::vector<std::string> paths;   // every file chosen, for a dialog that takes several
    };

    struct App
    {
        SDL_Window* window = nullptr;
        setup::State state = setup::State::NotInstalled;
        std::unique_ptr<Job> job;
        Picked picked;
        fs::path disc;                  // kept across a failed download, for the second attempt
        bool needsUpdateFile = false;   // the download failed: Install asks for the file
        std::string message;            // what the last job or action came to
        bool messageIsError = false;
        int messageFocus = -1;          // the profile screen's entry it belongs to
        bool profileScreen = false;
        std::vector<profile::Player> players;
        size_t player = 0;
        bool renaming = false;          // the keyboard types a profile's new name
        std::string newName;
        profile::Campaign campaign;
        // RANK and PRESTIGE as moved on the screen, by the player's stats
        // file (Player::file); 0 and -1 are not moved. They are written as the
        // screen is left -- BACK, or the window closed -- and only where they
        // differ from the file.
        struct Moved { int level = 0, prestige = -1; };
        std::map<fs::path, Moved> moved;
        update::Release release;        // the newer version a look found, if `newer`
        bool newer = false;
        std::string updated;            // the version this start is the first of
        bool gameRunning = false;       // looked up once a second while the profile screen shows
        bool reportScreen = false;
        bool graphicsScreen = false;
        bool mapsScreen = false;
        bool mapsWarning = false;       // CUSTOM MAPS is to be turned on: what that lets in, first
        std::vector<maps::Map> mapList; // read when the screen opens and after each change
        size_t mapIndex = 0;            // the one shown
        bool mapRemoving = false;       // REMOVE asks first

        void ReadMaps()
        {
            mapList = maps::List();
            mapIndex = mapList.empty() ? 0 : std::min(mapIndex, mapList.size() - 1);
        }

        // Moving or deleting a map's files under a running game would pull them from under it.
        bool MapsBusy()
        {
            if (!GameRunning()) return false;
            message = "The game is running. Close it to change its maps.";
            messageIsError = true;
            messageFocus = focus;
            return true;
        }
        // What FPS LIMIT goes through: the console's 60, two rates screens
        // commonly have, and none. A screen at another rate has its own in
        // place of the common one nearest to it, and until the player chooses,
        // the limit is the screen's.
        std::vector<int> fpsLimits{ settings::kConsoleFps, 120, 144, 0 };
        int screenFps = settings::kConsoleFps;

        void FindScreenRate()
        {
            if (const SDL_DisplayMode* mode = SDL_GetCurrentDisplayMode(SDL_GetDisplayForWindow(window)))
                screenFps = std::max(settings::kConsoleFps, int(std::lround(mode->refresh_rate)));
            if (std::find(fpsLimits.begin(), fpsLimits.end(), screenFps) == fpsLimits.end())
            {
                fpsLimits[std::abs(screenFps - fpsLimits[1]) <= std::abs(screenFps - fpsLimits[2]) ? 1 : 2] = screenFps;
                std::sort(fpsLimits.begin() + 1, fpsLimits.begin() + 3);
            }
            if (!settings::HasFpsLimit()) settings::SetFpsLimit(screenFps);
        }
        bool recording = false;         // the reported run goes on
        bool pendingReport = false;     // a recorded run was never reported: the start asks about it
        std::chrono::steady_clock::time_point recordingSince{};
        std::chrono::steady_clock::time_point gameChecked{};
        int focus = 0;
        bool focusPlaced = false;
        bool quit = false;

        void Ask(Action forAction, const char* what, const char* pattern, bool many = false)
        {
            {
                std::lock_guard lock(picked.lock);
                picked.answered = false;
                picked.forAction = forAction;
            }
            static SDL_DialogFileFilter filters[2];
            filters[0] = { what, pattern };
            filters[1] = { "All files", "*" };
            SDL_ShowOpenFileDialog(
                [](void* data, const char* const* list, int) {
                    auto* p = static_cast<Picked*>(data);
                    std::lock_guard lock(p->lock);
                    p->failed = !list;
                    p->path = (list && *list) ? *list : "";
                    p->paths.clear();
                    for (const char* const* file = list; file && *file; file++) p->paths.push_back(*file);
                    p->answered = true;
                },
                &picked, window, filters, 2, nullptr, many);
        }

        // A folder, the same way.
        void AskFolder(Action forAction, const fs::path& from)
        {
            {
                std::lock_guard lock(picked.lock);
                picked.answered = false;
                picked.forAction = forAction;
            }
            static std::string start;
            start = Utf8(from);
            SDL_ShowOpenFolderDialog(
                [](void* data, const char* const* list, int) {
                    auto* p = static_cast<Picked*>(data);
                    std::lock_guard lock(p->lock);
                    p->failed = !list;
                    p->path = (list && *list) ? *list : "";
                    p->paths.clear();
                    p->answered = true;
                },
                &picked, window, start.empty() ? nullptr : start.c_str(), false);
        }

        void Install(const fs::path& from, const fs::path& update)
        {
            message.clear();
            job = std::make_unique<Job>();
            job->disc = from;
            job->update = update;
            job->Start();
            focus = 0;
        }

        // Every shader of the game compiled for this computer, if they are not
        // yet (launcher/precompile.h), and then `then`. `device` is the one
        // the game said it draws on, or empty.
        void Compile(Action then, const std::string& device = "")
        {
            message.clear();
            messageIsError = false;
            job = std::make_unique<Job>();
            job->kind = Job::Kind::Compile;
            job->then = int(then);
            job->device = device;
            job->Start();
        }

        // A game started, once its shaders are compiled for the device `shadersFor`.
        void Launch(Action action, const std::string& shadersFor)
        {
            switch (action)
            {
            case Action::PlayCampaign:
            case Action::PlayMultiplayer:
            {
                const char* program = action == Action::PlayCampaign ? kCampaign : kMultiplayer;
                if (Start(program, shadersFor)) quit = true;
                else { message = std::string(program) + " could not be started: " + SDL_GetError(); messageIsError = true; }
                break;
            }
            case Action::ReportCampaign:
            case Action::ReportMultiplayer:
                if (report::Start(action == Action::ReportCampaign ? kCampaign : kMultiplayer, shadersFor, message))
                {
                    recording = true;
                    recordingSince = std::chrono::steady_clock::now();
                    message.clear();
                    focusPlaced = false;
                }
                else messageIsError = true;
                messageFocus = focus;
                break;
            default:
                break;
            }
        }

        void Do(Action action)
        {
            // While a job runs, it is all there is to do.
            if (job && action != Action::Cancel && action != Action::Quit) return;
            switch (action)
            {
            case Action::PlayCampaign:
            case Action::PlayMultiplayer:
                Compile(action);
                break;
            case Action::Install:
                // The disc's files are there already: the update is all that is missing.
                if (state == setup::State::NeedsUpdate) Install({}, {});
                else Ask(Action::Install, "Disc image", "iso");
                break;
            case Action::ChooseUpdate:
                Ask(Action::ChooseUpdate, "Title update", "*");
                break;
            case Action::Cancel:
                if (job) job->Cancel();
                break;
            case Action::Quit:
                quit = true;
                break;
            case Action::CheckUpdate:
                message.clear();
                job = std::make_unique<Job>();
                job->kind = Job::Kind::CheckUpdate;
                job->work = [this](setup::Progress&, std::string& error) {
                    switch (update::Look(release, error))
                    {
                    case update::Check::Newer: return setup::Result::Done;
                    case update::Check::UpToDate: return setup::Result::Cancelled;
                    case update::Check::Failed: break;
                    }
                    return setup::Result::Failed;
                };
                job->Start();
                break;
            case Action::InstallUpdate:
                // Its programs cannot be replaced under it.
                if (GameRunning())
                {
                    message = "The game is running. Close it to update.";
                    messageIsError = true;
                    break;
                }
                message.clear();
                job = std::make_unique<Job>();
                job->kind = Job::Kind::InstallUpdate;
                job->work = [release = release](setup::Progress& progress, std::string& error) { return update::Install(release, progress, error); };
                job->Start();
                break;
            case Action::ReportCampaign:
            case Action::ReportMultiplayer:
                if (GameRunning())
                {
                    message = "The game is running. Close it first: the report is of a run started here.";
                    messageIsError = true;
                    messageFocus = focus;
                }
                else Compile(action);
                break;
            case Action::Profile:
            case Action::Report:
            case Action::Graphics:
            case Action::Maps:
            case Action::Back:
            {
                const bool leavingProfile = profileScreen;
                gameRunning = action == Action::Profile && GameRunning();
                gameChecked = std::chrono::steady_clock::now();
                profileScreen = action == Action::Profile;
                reportScreen = action == Action::Report;
                graphicsScreen = action == Action::Graphics;
                mapsScreen = action == Action::Maps;
                mapsWarning = false;
                mapRemoving = false;
                mapIndex = 0;
                if (mapsScreen) ReadMaps();
                message.clear();
                messageIsError = false;
                focusPlaced = false;
                player = 0;
                if (leavingProfile) KeepMoved();
                ReadProfile();
                break;
            }
            case Action::SendPending:
                pendingReport = false;
                reportScreen = true;
                FinishReport();
                break;
            case Action::DiscardPending:
                pendingReport = false;
                report::Discard();
                focusPlaced = false;
                break;
            case Action::NextPlayer:
                player = (player + 1) % players.size();
                break;
            case Action::Fullscreen:
                settings::SetFullscreen(!settings::Fullscreen());
                break;
            case Action::CustomMaps:
                // Off at once; on only past the warning.
                if (settings::CustomMaps()) settings::SetCustomMaps(false);
                else { mapsWarning = true; focusPlaced = false; }
                break;
            case Action::CustomMapsOn:
            case Action::CustomMapsCancel:
                if (action == Action::CustomMapsOn) settings::SetCustomMaps(true);
                mapsWarning = false;
                focusPlaced = false;
                break;
            case Action::PcGame:
                message.clear();
                AskFolder(Action::PcGame, maps::PcGame());
                break;
            case Action::AddMap:
                if (MapsBusy()) break;
                message.clear();
                Ask(Action::AddMap, "Map files", "ff;arena;iwd", true);
                break;
            case Action::NextMap:
                if (!mapList.empty()) mapIndex = (mapIndex + 1) % mapList.size();
                message.clear();
                break;
            case Action::MapInGame:
                if (MapsBusy() || mapIndex >= mapList.size()) break;
                messageIsError = !maps::SetInGame(mapList[mapIndex], !mapList[mapIndex].inGame, message);
                messageFocus = focus;
                {
                    // The list is the game's first: find the map again where it went.
                    const std::string name = mapList[mapIndex].name;
                    ReadMaps();
                    for (size_t i = 0; i < mapList.size(); i++)
                        if (mapList[i].name == name) mapIndex = i;
                }
                break;
            case Action::RemoveMap:
                if (MapsBusy()) break;
                mapRemoving = true;
                focusPlaced = false;
                break;
            case Action::RemoveMapYes:
            case Action::RemoveMapNo:
                mapRemoving = false;
                focusPlaced = false;
                if (action == Action::RemoveMapYes && mapIndex < mapList.size() && !MapsBusy())
                {
                    messageIsError = !maps::Remove(mapList[mapIndex], message);
                    ReadMaps();
                    messageFocus = -1;
                }
                break;
            case Action::AspectRatio:
                settings::SetResolution(settings::AspectRatio() == settings::Ratio::Wide ? settings::Ratio::Wider
                                                                                         : settings::Ratio::Wide,
                                        settings::ResolutionRow());
                break;
            case Action::Resolution:
                settings::SetResolution(settings::AspectRatio(), (settings::ResolutionRow() + 1) % settings::kResolutions);
                break;
            case Action::Antialiasing:
            {
                // The choice after the one kept; one written by hand that is
                // none of them goes back to the first.
                static constexpr int kChoices[] = { 2, 4, 8, 0 };
                const int* kept = std::find(std::begin(kChoices), std::end(kChoices), settings::Msaa());
                settings::SetMsaa(kept == std::end(kChoices) || kept + 1 == std::end(kChoices) ? kChoices[0] : kept[1]);
                break;
            }
            case Action::FpsLimit:
            {
                // The choice after the one kept; one written by hand that is
                // none of them goes back to the first.
                const auto kept = std::find(fpsLimits.begin(), fpsLimits.end(), settings::FpsLimit());
                const bool last = kept == fpsLimits.end() || kept + 1 == fpsLimits.end();
                settings::SetFpsLimit(last ? fpsLimits.front() : kept[1]);
                break;
            }
            case Action::Fov:
                // Choosing goes one step wider, and round to the console's.
                settings::SetFov(settings::Fov() >= settings::kWidestFov ? settings::kConsoleFov : settings::Fov() + settings::kFovStep);
                break;
            case Action::Rename:
                renaming = true;
                newName = players[player].name;
                message.clear();
                SDL_StartTextInput(window);
                break;
            case Action::Rank:
            case Action::Prestige:
                // Left and right move them (Window()).
                break;
            case Action::PlayAs:
            case Action::UnlockEverything:
            case Action::UnlockMissions:
            case Action::AllStars:
                Edit(action);
                break;
            case Action::None:
                break;
            }
        }

        // The keyboard while a profile is renamed.
        void Type(const SDL_Event& event)
        {
            if (event.type == SDL_EVENT_TEXT_INPUT)
            {
                for (const char* c = event.text.text; *c; c++)
                    if (newName.size() < 15 && (std::isalnum(static_cast<unsigned char>(*c)) || *c == ' ')) newName += *c;
                return;
            }
            if (event.type != SDL_EVENT_KEY_DOWN) return;
            if (event.key.key == SDLK_BACKSPACE && !newName.empty()) newName.pop_back();
            const bool keep = event.key.key == SDLK_RETURN || event.key.key == SDLK_KP_ENTER;
            if (!keep && event.key.key != SDLK_ESCAPE) return;
            renaming = false;
            SDL_StopTextInput(window);
            if (!keep) return;
            std::string error;
            const uint64_t id = players[player].id;
            const bool done = profile::Rename(players[player], newName, error);
            message = done ? "The profile is now " + newName + "." : error;
            messageIsError = !done;
            messageFocus = focus;
            ReadProfile();
            for (size_t i = 0; i < players.size(); i++)
                if (players[i].id == id) player = i;
        }

        void ReadProfile()
        {
            players = profile::Players();
            player = std::min(player, players.empty() ? 0 : players.size() - 1);
            campaign = profile::ReadCampaign();
        }

        // The rank and prestige the player's rows show: as moved, or as the file has them.
        int ShownLevel(const profile::Player& p) const
        {
            const auto it = moved.find(p.file);
            return it != moved.end() && it->second.level ? it->second.level : p.level;
        }
        int ShownPrestige(const profile::Player& p) const
        {
            const auto it = moved.find(p.file);
            return it != moved.end() && it->second.prestige >= 0 ? it->second.prestige : p.prestige;
        }

        // What was moved, written: a profile whose rank and prestige are
        // where they were is left untouched.
        void KeepMoved()
        {
            if (moved.empty()) return;
            std::map<fs::path, Moved> keep;
            keep.swap(moved);
            ReadProfile();
            std::vector<std::pair<const profile::Player*, Moved>> changes;
            for (const auto& [file, to] : keep)
                for (const profile::Player& p : players)
                    if (p.file == file)
                    {
                        const Moved change{ to.level && to.level != p.level ? to.level : 0,
                                            to.prestige >= 0 && to.prestige != p.prestige ? to.prestige : -1 };
                        if (change.level || change.prestige >= 0) changes.push_back({ &p, change });
                    }
            if (changes.empty()) return;
            // The game writes the profile it has in memory as it ends, over the change.
            if (GameRunning())
            {
                message = "The game is running, so the rank and prestige were not changed. Close it first.";
                messageIsError = true;
                messageFocus = -1;
                return;
            }
            for (const auto& [p, change] : changes)
            {
                std::string error;
                if (profile::SetRank(*p, change.level, change.prestige, error)) continue;
                message = error;
                messageIsError = true;
                messageFocus = -1;
            }
            ReadProfile();
        }

        // One change to the profile, and what to say about it.
        void Edit(Action action)
        {
            std::string error;
            bool done = false;
            const profile::Player* who = players.empty() ? nullptr : &players[player];
            // The list is the newest first, and a change makes its file the
            // newest: the player is found again by the file.
            const fs::path file = who ? who->file : fs::path();
            const uint64_t id = who ? who->id : 0;
            switch (action)
            {
            case Action::PlayAs:
                done = who && profile::PlayAs(*who, error);
                message = who ? who->name + " plays at the first controller from the game's next start." : "";
                break;
            case Action::UnlockEverything:
                done = who && profile::UnlockEverything(*who, error);
                message = "Every challenge is done, and every title, emblem and killstreak unlocked.";
                break;
            case Action::UnlockMissions:
                done = profile::UnlockMissions(error);
                message = "Every campaign mission is unlocked.";
                break;
            case Action::AllStars:
                done = profile::AllStars(error);
                message = "Every Special Ops mission has its three stars.";
                break;
            default:
                break;
            }
            if (!done) message = error;
            messageIsError = !done;
            messageFocus = focus;
            ReadProfile();
            for (size_t i = 0; i < players.size(); i++)
                if (id ? players[i].id == id : players[i].file == file) player = i;
        }

        std::vector<Entry> ReportEntries() const
        {
            std::vector<Entry> entries;
            entries.reserve(kMostEntries);   // add() hands out pointers into it: it must not move
            auto add = [&](Action action, const char* label, const char* heading, std::string text) {
                Entry entry;
                entry.shown.label = label;
                entry.action = action;
                entry.heading = heading;
                entry.text = std::move(text);
                entries.push_back(std::move(entry));
                return &entries.back();
            };
            if (recording)
            {
                add(Action::None, "RECORDING", "THE GAME IS RUNNING",
                    "Play until the problem shows, then quit the game.\n\nThe report is made when the game has closed. "
                    "If the game closes by itself, that is recorded too.");
                return entries;
            }
            const std::string how =
                "The game starts and keeps a log of the run. Play until the problem shows, then quit the game.\n\n"
                "The launcher then writes one report file and opens the project's new-issue page on GitHub with your "
                "system's description filled in. You describe what happened, drag the file in, and submit. "
                "That needs a GitHub account.\n\n"
                "The report is public. Your name, network addresses and folder names are taken out of it.";
            std::error_code ec;
            const bool installed = state == setup::State::Installed;
            Entry* campaign = add(Action::ReportCampaign, "CAMPAIGN", "REPORT A BUG IN THE CAMPAIGN", how);
            campaign->shown.enabled = installed && fs::is_regular_file(kCampaign, ec);
            Entry* multiplayer = add(Action::ReportMultiplayer, "MULTIPLAYER", "REPORT A BUG IN THE MULTIPLAYER", how);
            multiplayer->shown.enabled = installed && fs::is_regular_file(kMultiplayer, ec);
            for (Entry* entry : { campaign, multiplayer })
                if (!entry->shown.enabled) { entry->action = Action::None; entry->text = "Install the game first."; }
            add(Action::Back, "BACK", "", "Reports are kept in the reports folder beside the launcher.")->shown.ruleAbove = true;
            return entries;
        }

        // The reported run has ended: the file, the page, and what to do with them.
        void FinishReport()
        {
            report::Made made;
            recording = false;
            focusPlaced = false;
            messageFocus = -1;
            messageIsError = !report::Make(made, message);
            if (messageIsError) return;
            const std::string file = Utf8(made.file);
            message = report::Open(made)
                ? "The report is\n" + file + "\n\nThe issue page has opened in your browser. Describe what happened, "
                  "drag that file into the text box, and submit."
                : "The report is\n" + file + "\n\nNo browser could be opened. Attach that file to a new issue at\n"
                  "github.com/PaulCombal/mw2-recompiled/issues";
        }

        std::vector<Entry> ProfileEntries() const
        {
            std::vector<Entry> entries;
            entries.reserve(kMostEntries);   // add() hands out pointers into it: it must not move
            auto add = [&](Action action, std::string label, bool enabled, const char* heading, std::string text) {
                Entry entry;
                entry.shown.label = std::move(label);
                entry.shown.enabled = enabled;
                entry.action = enabled ? action : Action::None;
                entry.heading = heading;
                entry.text = std::move(text);
                entries.push_back(std::move(entry));
                return &entries.back();
            };
            // A profile made on the sign-in screen has no stats to change
            // until its first match has ended.
            const bool any = !players.empty() && !players[player].file.empty() && !gameRunning;
            std::string who = "No multiplayer profile yet: play the multiplayer once first.";
            int level = 1, prestige = 0;
            bool named = false, movedHere = false;
            if (!players.empty())
            {
                const profile::Player& p = players[player];
                level = ShownLevel(p);
                prestige = ShownPrestige(p);
                movedHere = level != p.level || prestige != p.prestige;
                named = p.id != 0;
                auto rank = [](bool offline, int level, int prestige, const std::string& played) {
                    return std::string(offline ? "Offline" : "Online") + ": rank " + std::to_string(level) + ", prestige " +
                           std::to_string(prestige) + ", last played " + played + ".";
                };
                who = named ? p.name + (p.first ? ", player 1." : ".") : std::string("Stats of no profile.");
                if (p.file.empty()) who += "\nNot played yet.";
                else who += "\n" + rank(p.offline, p.level, p.prestige, p.played);
                for (const profile::Player::Other& other : p.also)
                    who += "\n" + rank(other.offline, other.level, other.prestige, other.played);
            }
            // The game keeps the profile in memory and writes it when it ends,
            // over anything changed here meanwhile.
            const std::string close = gameRunning ? "\n\nTHE GAME IS RUNNING. Close it to change the profile." : "";
            if (players.size() > 1)
                add(Action::NextPlayer, "PLAYER " + std::to_string(player + 1) + " OF " + std::to_string(players.size()), true, "PLAYER",
                    who + "\n\nSeveral players have played here. Choose to go to the next one.");
            add(Action::Rename, "RENAME", named && !gameRunning, "NAME",
                renaming ? "New name: " + newName + "_\n\nENTER keeps it, ESC leaves the name as it was."
                         : who + "\n\n" + (named ? "Type a new name for this profile with the keyboard. On Steam, other players see the Steam name."
                                                  : "No profile owns these stats any more: they are from a version of the game before profiles, or from a profile since deleted.") + close);
            const bool isFirst = named && players[player].first;
            add(Action::PlayAs, isFirst ? "PLAYER 1" : "PLAY AS", named && !isFirst && !gameRunning, "PLAYER 1",
                who + "\n\n" + (isFirst ? "This profile plays at the first controller: the campaign's and the multiplayer's player. Choose another profile here to play as that one."
                                        : named ? "Puts this profile at the first controller, from the game's next start."
                                                : "Only a profile can be put at the first controller.") + close);
            // Moved, they are written as the screen is left.
            const std::string kept = movedHere ? "\n\nThe change is kept when you go BACK." : "";
            add(Action::Rank, "RANK " + std::to_string(level), any, "MULTIPLAYER RANK",
                who + "\n\nLeft and right change it, from 1 to " + std::to_string(profile::MaxLevel()) +
                    ", online and offline. A rank unlocks the weapons, perks and equipment of its level and those below; "
                    "a lower one locks those above it again. " + std::to_string(profile::MaxLevel()) + " unlocks all of them. "
                    "The attachments and camouflages are the challenges', and stay." + kept + close);
            add(Action::Prestige, "PRESTIGE " + std::to_string(prestige), any, "PRESTIGE",
                who + "\n\nLeft and right change it, from 0 to " + std::to_string(profile::MaxPrestige()) + ". The rank stays." + kept + close);
            add(Action::UnlockEverything, "UNLOCK EVERYTHING", any, "CHALLENGES AND UNLOCKS",
                who + "\n\nMarks every challenge as done, which gives every attachment and camouflage, and unlocks every title, emblem and killstreak." + close);

            const std::string progress = campaign.found
                ? "Special Ops: " + std::to_string(campaign.stars) + " of 69 stars."
                : std::string("No campaign profile yet: start the campaign once first.");
            add(Action::UnlockMissions, "UNLOCK ALL MISSIONS", campaign.found && !gameRunning, "CAMPAIGN",
                progress + "\n\nOpens every mission of the campaign in the mission list." + close)->shown.ruleAbove = true;
            add(Action::AllStars, "ALL SPEC OPS STARS", campaign.found && !gameRunning, "SPECIAL OPS",
                progress + "\n\nGives every Special Ops mission its three stars, which opens all of them." + close);
            add(Action::Back, "BACK", true, "", "A file is copied to <name>.backup, beside it under saves/, before its first change.")->shown.ruleAbove = true;
            return entries;
        }

        // The graphics screen.
        std::vector<Entry> GraphicsEntries() const
        {
            std::vector<Entry> entries;
            entries.reserve(kMostEntries);   // add() hands out pointers into it: it must not move
            auto add = [&](Action action, std::string label, const char* heading, std::string text) {
                Entry entry;
                entry.shown.label = std::move(label);
                entry.action = action;
                entry.heading = heading;
                entry.text = std::move(text);
                entries.push_back(std::move(entry));
                return &entries.back();
            };
            const bool fullscreen = settings::Fullscreen();
            add(Action::Fullscreen, std::string("FULLSCREEN ") + (fullscreen ? "ON" : "OFF"), "FULLSCREEN",
                std::string("Whether the game starts fullscreen: now ") + (fullscreen ? "it does, at the screen's own resolution." : "it starts in a window.") +
                "\n\nChoose to switch it. While playing, F8 switches between the two."
                "\n\nIt applies to the campaign and the multiplayer, from the next time either is started.");
            const bool wider = settings::AspectRatio() == settings::Ratio::Wider;
            add(Action::AspectRatio, std::string("ASPECT RATIO ") + (wider ? "16:10" : "16:9"), "ASPECT RATIO",
                std::string("The shape of the game's picture: now ") + (wider ? "16:10." : "16:9, the console's own.") +
                "\n\nChoose to switch between 16:9 and 16:10. A screen or window of another shape shows black bars. "
                "The menus and the HUD follow the shape. On 16:10, the videos show whole between thin black bars, "
                "and the full-screen pictures fill the screen, a little of their sides cut."
                "\n\nIt applies to the campaign and the multiplayer, from the next time either is started.");
            const settings::Resolution& resolution = settings::CurrentResolution();
            const auto& rows = settings::kResolutionTable[int(settings::AspectRatio())];
            std::string sizes;
            for (int row = 0; row < settings::kResolutions; row++)
                sizes += std::string(row ? ", " : "") + rows[row].name;
            add(Action::Resolution, std::string("RESOLUTION ") + resolution.name, "RESOLUTION",
                std::string("The size the game draws at, whatever the window's: now ") + resolution.name +
                (resolution.width == 1280 && resolution.height == 720 && resolution.scale == 1 ? ", the console's own." : ".") +
                "\n\nChoose to go to the next: " + sizes + ". A larger one is sharper and needs a faster graphics card. "
                "The best is the screen's own: a smaller one is enlarged to fit it, a larger one made smaller."
                "\n\nIt applies to the campaign and the multiplayer, from the next time either is started.");
            const int msaa = settings::Msaa();
            add(Action::Antialiasing, "ANTIALIASING " + (msaa ? std::to_string(msaa) + "X" : std::string("OFF")), "ANTIALIASING",
                std::string("How smooth the edges of what the game draws are: now ") +
                (msaa == 2 ? "2x, the console's own." : msaa ? std::to_string(msaa) + "x, where the console's is 2x." : "off, with jagged edges.") +
                "\n\nChoose to go to the next: 2x, 4x, 8x, off. More is smoother and needs a faster graphics card; a graphics card "
                "that can't draw as many uses the most it can. Off is the fastest."
                "\n\nIt applies to the campaign and the multiplayer, from the next time either is started.");
            const int limit = settings::FpsLimit();
            add(Action::FpsLimit, "FPS LIMIT " + (limit ? std::to_string(limit) : std::string("NONE")), "FPS LIMIT",
                std::string("How many frames a second the game draws at most: now ") +
                (limit == settings::kConsoleFps ? "60, the console's own." : limit ? std::to_string(limit) + "." : "as many as the computer manages.") +
                "\n\nChoose to go to the next: " + std::to_string(fpsLimits[0]) + ", " + std::to_string(fpsLimits[1]) + ", " +
                std::to_string(fpsLimits[2]) + ", none. This screen shows " + std::to_string(screenFps) + " pictures a second. More than 60 "
                "makes the game answer the controller sooner, and looks smoother on a screen that shows more. It needs a faster "
                "computer, and the game was made for 60: if something misbehaves, go back to it."
                "\n\nIt applies to the campaign and the multiplayer, from the next time either is started.");
            const int fov = settings::Fov();
            add(Action::Fov, "FOV " + std::to_string(fov), "FIELD OF VIEW",
                std::string("How wide the game's view is: now ") + std::to_string(fov) +
                (fov == settings::kConsoleFov ? ", the console's own." : ", where the console's is 65.") +
                " Left and right change it. A wider view shows more to the sides and makes what is ahead smaller, "
                "aiming down a sight included; it needs a faster computer. "
                "It applies to the campaign and the multiplayer, from the next time either is started.");
            add(Action::Back, "BACK", "", "")->shown.ruleAbove = true;
            return entries;
        }

        // The maps screen.
        std::vector<Entry> MapsEntries() const
        {
            std::vector<Entry> entries;
            entries.reserve(kMostEntries);   // add() hands out pointers into it: it must not move
            auto add = [&](Action action, std::string label, const char* heading, std::string text) {
                Entry entry;
                entry.shown.label = std::move(label);
                entry.action = action;
                entry.heading = heading;
                entry.text = std::move(text);
                entries.push_back(std::move(entry));
                return &entries.back();
            };
            if (mapsWarning)
            {
                const char* const warning =
                    "The game's own maps are signed, and the multiplayer checks each one before it loads it: a file "
                    "somebody changed is refused. A custom map is made by somebody else, and nothing checks it.\n\n"
                    "A map is not only pictures and sounds. It holds scripts the game runs, and data the game reads "
                    "as it is, trusting that nothing in it is wrong. A map made to do harm, or only a broken one, can "
                    "crash the game or change how a match plays. At worst it can take the game over, and with it "
                    "reach whatever your account on this computer can: your files and your network.\n\n"
                    "Only add maps from people you trust, downloaded from where they published them. The game's own "
                    "maps stay checked, and turning custom maps off again makes the game refuse the others.";
                add(Action::CustomMapsCancel, "GO BACK", "BEFORE CUSTOM MAPS ARE ON", warning);
                add(Action::CustomMapsOn, "TURN ON", "BEFORE CUSTOM MAPS ARE ON", warning)->shown.ruleAbove = true;
                return entries;
            }
            if (mapRemoving && mapIndex < mapList.size())
            {
                const maps::Map& map = mapList[mapIndex];
                const std::string text = "The launcher's copy of it is deleted. The files you added it from stay where they are, "
                                         "and adding them again brings it back.";
                std::string heading = "REMOVE " + Upper(map.longname) + "?";
                Entry* keep = add(Action::RemoveMapNo, "KEEP IT", "", text);
                Entry* remove = add(Action::RemoveMapYes, "REMOVE", "", text);
                for (Entry* entry : { keep, remove })
                {
                    entry->heading = heading;
                    entry->picture = Utf8(map.picture);
                }
                remove->shown.ruleAbove = true;
                return entries;
            }
            const bool on = settings::CustomMaps();
            add(Action::CustomMaps, std::string("CUSTOM MAPS ") + (on ? "ON" : "OFF"), "CUSTOM MAPS",
                std::string("Whether the multiplayer loads maps other people made, such as maps converted from the PC "
                            "game: now ") + (on ? "it does." : "it loads only the game's own.") +
                "\n\nChoose to switch it" + (on ? "." : ": a warning comes first, about what a map from somebody else can do.") +
                "\n\nIt applies to the multiplayer from the next time it is started.");
            {
                const fs::path game = maps::PcGame();
                add(Action::PcGame, std::string("PC GAME ") + (game.empty() ? "NONE" : "SET"), "PC GAME",
                    "Maps made for the PC game often use pictures and sounds of the PC game's own maps without "
                    "carrying them, as every PC player has them. When you have the PC game too (Call of Duty: Modern "
                    "Warfare 2 on Steam), a map added here takes them from it; without it, plain ones stand in: "
                    "magenta for a picture, silence for a sound.\n\n" +
                        (!game.empty() ? "The PC game's files used: " + Utf8(game) + "."
                         : settings::PcGame()
                             ? std::string("None is used: MW2_PC_GAME is left empty in the .env file beside the launcher.")
                             : std::string("No copy of the PC game was found in Steam's folders.")) +
                        "\n\nChoose to pick the folder of another copy, the one with main in it.");
            }
            add(Action::AddMap, "ADD MAP", "ADD MAP",
                "Choose one of the map's files: its others, <map>.ff, <map>_load.ff, <map>.arena and <map>.iwd, are "
                "taken from the same folder. A map made for the PC game (IW4x) is converted for the Xbox 360, which takes a minute or "
                "so; one converted already is added as it is. The .iwd gives its pictures, and its picture here; "
                "what the map takes from the PC game itself comes from PC GAME. The launcher keeps a copy of the map: "
                "the files you chose stay where they are.\n\n"
                "The game has room for " + std::to_string(maps::kInGame) + " custom maps. A map added while it has them "
                "waits in the maps folder beside the launcher until another is taken out.");
            if (mapList.empty()) { add(Action::Back, "BACK", "", "")->shown.ruleAbove = true; return entries; }

            // One map at a time, and what can be done with it.
            const maps::Map& map = mapList[std::min(mapIndex, mapList.size() - 1)];
            const size_t inGame = maps::InGame(mapList);
            std::string about = map.description.empty() ? std::string() : map.description + "\n\n";
            about += map.inGame ? (on ? "In the game: in a private match, GAME SETUP, the map, then BONUS MAPS."
                                      : "In the game, once custom maps are on.")
                                : "Not in the game: its files wait in the maps folder beside the launcher.";
            about += "\n" + std::to_string(inGame) + " of the game's " + std::to_string(maps::kInGame) + " places are taken.";
            const std::string heading = Upper(map.longname);
            Entry* pick = add(Action::NextMap, heading, "", about);
            pick->shown.tag = std::to_string(mapIndex + 1) + " OF " + std::to_string(mapList.size());
            pick->shown.ruleAbove = true;
            Entry* place = add(Action::MapInGame, std::string("IN THE GAME ") + (map.inGame ? "YES" : "NO"), "",
                               about + "\n\nChoose to " + (map.inGame ? "take it out of the game. Its files are kept."
                                                                          : inGame >= maps::kInGame ? "put it in the game: first take another out, the game has room for " + std::to_string(maps::kInGame) + "."
                                                                                                    : "put it in the game."));
            Entry* remove = add(Action::RemoveMap, "REMOVE", "", "Deletes the launcher's copy of the map. The files you added it from stay "
                                                                   "where they are. A question comes first.");
            for (Entry* entry : { pick, place, remove })
            {
                entry->heading = heading;
                entry->picture = Utf8(map.picture);
            }
            add(Action::Back, "BACK", "", "")->shown.ruleAbove = true;
            return entries;
        }

        // A file the dialog returned, or one dropped on the window.
        void Take(Action forAction, const std::string& path, const std::vector<std::string>& paths = {})
        {
            if (job) return;
            if (forAction == Action::AddMap)
            {
                std::vector<fs::path> files;
                for (const std::string& file : paths) files.push_back(FromUtf8(file));
                if (files.empty()) return;
                messageFocus = focus;
                if (maps::Converts(files))
                {
                    // A map made for the PC game is converted first: a minute or so.
                    message.clear();
                    job = std::make_unique<Job>();
                    job->kind = Job::Kind::AddMap;
                    // (The PC game's folder is looked up here: the job's thread doesn't touch .env.)
                    job->work = [files, game = maps::PcGame()](setup::Progress& progress, std::string& said) {
                        if (maps::Add(files, said, &progress, game)) return setup::Result::Done;
                        return progress.cancel ? setup::Result::Cancelled : setup::Result::Failed;
                    };
                    job->Start();
                    return;
                }
                messageIsError = !maps::Add(files, message);
                ReadMaps();
                return;
            }
            if (path.empty()) return;
            if (forAction == Action::PcGame)
            {
                messageFocus = focus;
                messageIsError = !maps::SetPcGame(FromUtf8(path), message);
                return;
            }
            if (forAction == Action::ChooseUpdate) Install(state == setup::State::NeedsUpdate ? fs::path() : disc, FromUtf8(path));
            else { disc = FromUtf8(path); Install(disc, {}); }
        }

        void Poll()
        {
            std::string path;
            std::vector<std::string> paths;
            Action forAction = Action::None;
            {
                std::lock_guard lock(picked.lock);
                if (picked.answered)
                {
                    picked.answered = false;
                    forAction = picked.forAction;
                    path = picked.path;
                    paths = picked.paths;
                    if (picked.failed)
                    {
                        message = std::string("No file dialog could be opened (") + SDL_GetError() + ")." +
                                  (forAction == Action::AddMap ? std::string(" Copy the map's three files into the game folder beside the launcher instead.")
                                   : forAction == Action::PcGame ? std::string(" Write MW2_PC_GAME=<the PC game's folder> in the .env file beside the launcher instead.")
                                                               : std::string(" Drop the file on this window, or install from a terminal:\n"
                                                                             "mw2-launcher --install path/to/disc.iso"));
                        messageIsError = true;
                        messageFocus = focus;
                    }
                }
            }
            if (forAction != Action::None) Take(forAction, path, paths);

            // Two seconds in, the game has had time to appear among the processes;
            // the title one starts from its menus is running before it ends itself.
            if (recording && std::chrono::steady_clock::now() - gameChecked > std::chrono::seconds(1))
            {
                gameChecked = std::chrono::steady_clock::now();
                if (gameChecked - recordingSince > std::chrono::seconds(2) && !GameRunning()) FinishReport();
            }

            if (profileScreen && std::chrono::steady_clock::now() - gameChecked > std::chrono::seconds(1))
            {
                const bool was = gameRunning;
                gameRunning = GameRunning();
                gameChecked = std::chrono::steady_clock::now();
                // It has ended and written its files: read them again.
                if (was && !gameRunning) ReadProfile();
            }

            if (job && job->finished && job->kind == Job::Kind::Compile)
            {
                job->thread.join();
                const precompile::Result result = job->compiled;
                const Action then = Action(job->then);
                job.reset();
                focusPlaced = false;
                if (result.ok)
                {
                    if (result.noLibraries)
                        message = result.device + " can compile a shader only with the state of the draw it is "
                                  "for, so the game compiles each as it first meets it, and may stutter then.";
                    else if (!result.current)
                        message = "Every shader of the game, " + std::to_string(result.shaders) +
                                  ", is compiled for this computer.";
                    messageIsError = false;
                    if (then != Action::None) Launch(then, result.deviceId);
                }
                else if (result.cancelled)
                {
                    message = then == Action::None ? "The shaders were not compiled. The game compiles them before it is played."
                                                   : "The shaders were not compiled, so the game was not started: it is "
                                                     "played only with all of them compiled.";
                    messageIsError = true;
                }
                else
                {
                    message = "The shaders could not be compiled, so the game cannot be played: " + result.error;
                    messageIsError = true;
                }
            }

            if (job && job->finished && job->kind == Job::Kind::AddMap)
            {
                job->thread.join();
                message = job->error;
                messageIsError = job->result != setup::Result::Done;
                job.reset();
                ReadMaps();
                // Back on ADD MAP, which the message is shown on.
                focus = std::max(0, messageFocus);
                focusPlaced = true;
            }

            if (job && job->finished && job->kind != Job::Kind::Install)
            {
                job->thread.join();
                const setup::Result result = job->result;
                messageIsError = result == setup::Result::Failed;
                if (job->kind == Job::Kind::CheckUpdate)
                {
                    newer = result == setup::Result::Done;
                    message = newer                               ? ""
                              : result == setup::Result::Failed   ? job->error
                                                                  : std::string("This is the newest version, ") + update::Current() + ".";
                }
                else if (result == setup::Result::Done)
                {
                    // The new launcher takes over from here.
                    if (Start(kLauncher)) quit = true;
                    else message = "The new version is installed. Start the launcher again.";
                    newer = false;
                }
                else message = result == setup::Result::Cancelled ? "The update was stopped. Nothing was changed." : job->error;
                job.reset();
            }

            if (job && job->finished)
            {
                job->thread.join();
                const setup::Result result = job->result;
                needsUpdateFile = result == setup::Result::NoUpdate;
                messageIsError = result == setup::Result::Failed || result == setup::Result::NoUpdate;
                if (result == setup::Result::Done) message = "The game is installed.";
                else if (result == setup::Result::Cancelled) message = "The install was stopped. Starting it again carries on where it left off.";
                else message = job->error;
                if (needsUpdateFile)
                    message += std::string("\n\nDownload it yourself from\n") + setup::UpdateUrl() +
                               "\nthen choose the file with CHOOSE UPDATE FILE.";
                job.reset();
                state = setup::Detect();
                sound::Load();
                focusPlaced = false;
                // Then every shader of the game, while the player is here.
                if (result == setup::Result::Done) Compile(Action::None);
            }
        }

        // A look at the compiled shaders that finds them up to date is over in
        // a moment: the screen stays as it was meanwhile.
        bool JobShown() const { return job && (job->kind != Job::Kind::Compile || job->compile.checked); }

        std::vector<Entry> Entries() const
        {
            if (pendingReport)
            {
                // The launcher was ended with the game, as Steam's "Exit game" does.
                std::vector<Entry> entries(2);
                entries[0].shown.label = "YES";
                entries[0].action = Action::SendPending;
                entries[1].shown.label = "NO";
                entries[1].action = Action::DiscardPending;
                for (Entry& entry : entries)
                {
                    entry.heading = "A BUG REPORT WASN'T SENT";
                    entry.text = "The game was recorded for a bug report, and the report was not sent. Send it now?\n\n"
                                 "YES makes the report and opens the page to send it on. NO throws the recording away.";
                }
                return entries;
            }
            std::vector<Entry> entries;
            entries.reserve(kMostEntries);   // add() hands out pointers into it: it must not move
            auto add = [&](Action action, std::string label, bool enabled, const char* heading, std::string text) {
                Entry entry;
                entry.shown.label = std::move(label);
                entry.shown.enabled = enabled;
                entry.action = enabled ? action : Action::None;
                entry.heading = heading;
                entry.text = std::move(text);
                entries.push_back(std::move(entry));
                return &entries.back();
            };
            if (JobShown())
            {
                add(Action::Cancel, "CANCEL", true,
                    job->kind == Job::Kind::Install    ? "INSTALLING"
                    : job->kind == Job::Kind::Compile  ? "SHADERS"
                    : job->kind == Job::Kind::AddMap   ? "ADDING A MAP"
                                                       : "UPDATES",
                    "");
                return entries;
            }
            if (profileScreen) return ProfileEntries();
            if (reportScreen) return ReportEntries();
            if (graphicsScreen) return GraphicsEntries();
            if (mapsScreen) return MapsEntries();
            const bool installed = state == setup::State::Installed;
            std::error_code ec;
            const bool campaign = fs::is_regular_file(kCampaign, ec), multiplayer = fs::is_regular_file(kMultiplayer, ec);
            auto play = [&](bool present, const char* program, const char* what) {
                if (!installed) return std::string("Install the game first.");
                if (!present) return std::string(program) + " is not beside the launcher.";
                return std::string(what);
            };
            add(Action::PlayCampaign, "PLAY CAMPAIGN", installed && campaign, "CAMPAIGN",
                play(campaign, kCampaign, "The campaign and Special Ops, alone or in split screen."));
            add(Action::PlayMultiplayer, "PLAY MULTIPLAYER", installed && multiplayer, "MULTIPLAYER",
                play(multiplayer, kMultiplayer, "Multiplayer: split screen, system link and private matches."));

            // Until the game is whole, installing it is the next thing to do.
            auto install = [&] {
                if (needsUpdateFile)
                    return add(Action::ChooseUpdate, "CHOOSE UPDATE FILE", true, "TITLE UPDATE 6",
                               "The update could not be downloaded. Choose the file you downloaded yourself.");
                if (state == setup::State::NeedsUpdate)
                    return add(Action::Install, "UPDATE GAME", true, "UPDATE",
                               "The game is installed from the disc, and this version plays title update 6.\n\n"
                               "The update is downloaded (2 MB) and applied to the installed game. Your disc is not needed.");
                return add(Action::Install, installed ? "REINSTALL" : "INSTALL GAME", true, "INSTALL",
                           std::string("Choose the image of your Xbox 360 disc (.iso), or drop it on this window.\n\n"
                                       "Its files, about 7 GB, are copied into the game folder beside the launcher") +
                               (setup::UsesUpdate() ? ", and title update 6 is downloaded (2 MB) and applied." : ".") +
                               "\n\nThe disc is the USA/Europe one, version 1.0.557.");
            };
            const bool reinstall = installed && !needsUpdateFile;
            if (!reinstall) install()->shown.ruleAbove = true;

            // What the game is played with.
            add(Action::Graphics, "GRAPHICS", true, "GRAPHICS", "Fullscreen or a window, the size the game draws at, its antialiasing, how many frames a second and how wide its view is.")->shown.ruleAbove = true;
            add(Action::Profile, "PROFILES", true, "PROFILES",
                "Set the multiplayer rank and prestige, unlock everything, and open the campaign's and Special Ops' missions.");
            add(Action::Maps, "MAPS", true, "MAPS",
                "Custom maps: maps other people made, such as maps converted from the PC game, in the multiplayer. "
                "In beta: some maps don't convert, and some that do don't play yet.")->shown.tag = "BETA";

            // The copy itself. A build nobody released has no version to compare.
            const bool released = *update::Current() != 0;
            if (newer)
                add(Action::InstallUpdate, "UPDATE TO " + release.version, true, "UPDATES",
                    "Version " + release.version + " is out; this is " + update::Current() + ".\n\n"
                    "It is downloaded (" + std::to_string((release.size + 500000) / 1000000) + " MB) and put in place of this one, "
                    "and the launcher starts again. The game's files and your saves stay as they are.")->shown.ruleAbove = true;
            else
                add(Action::CheckUpdate, "CHECK FOR UPDATES", released, "UPDATES",
                    released ? std::string("Look for a newer version than this one, ") + update::Current() + "."
                             : std::string("This build was not made as a release, so there is no version to compare."))->shown.ruleAbove = true;
            if (reinstall) install();
            add(Action::Report, "REPORT A BUG", true, "REPORT A BUG",
                "Something wrong with the game? Send the game's report data to GitHub.\n\nA GitHub account is required.");
            add(Action::Quit, "QUIT", true, "", "")->shown.ruleAbove = true;
            return entries;
        }
    };

    // `then` is what to do at once, `device` the one the game draws on (--play).
    int Window(const std::string& updated, Action then, const std::string& device)
    {
        if (!SDL_Init(SDL_INIT_VIDEO | SDL_INIT_GAMEPAD))
        {
            std::fprintf(stderr, "No window could be opened (%s). Install from a terminal: mw2-launcher --install <disc>\n", SDL_GetError());
            return 1;
        }
        // The design is 1280x720; a smaller desktop gets it scaled down.
        float scale = 1.0f;
        SDL_Rect bounds;
        if (SDL_GetDisplayUsableBounds(SDL_GetPrimaryDisplay(), &bounds))
            scale = std::min(1.0f, std::min(bounds.w / 1320.0f, bounds.h / 780.0f));
        App app;
        SDL_Renderer* renderer = nullptr;
        if (!SDL_CreateWindowAndRenderer("Modern Warfare 2", int(1280 * scale), int(720 * scale), 0, &app.window, &renderer))
        {
            SDL_ShowSimpleMessageBox(SDL_MESSAGEBOX_ERROR, "Modern Warfare 2", SDL_GetError(), nullptr);
            return 1;
        }
        if (SDL_Surface* icon = SDL_LoadBMP_IO(SDL_IOFromConstMem(kIcon, kIconSize), true))
        {
            SDL_SetWindowIcon(app.window, icon);
            SDL_DestroySurface(icon);
        }
        SDL_SetRenderVSync(renderer, 1);
        // In front, with the keyboard's focus: SDL hears no controller for a
        // window that has not got it, and a window the system opened behind
        // another -- it does when the program starting it was not in front --
        // answers to nothing but the mouse until it is clicked.
        SDL_RaiseWindow(app.window);
        app.FindScreenRate();

        IMGUI_CHECKVERSION();
        ImGui::CreateContext();
        ImGuiIO& io = ImGui::GetIO();
        io.IniFilename = nullptr;       // nothing is kept between runs
        io.ConfigFlags |= ImGuiConfigFlags_NavEnableKeyboard | ImGuiConfigFlags_NavEnableGamepad;
        // A held direction goes through the entries at a pace each one can be
        // seen and heard at: Dear ImGui's own, 20 a second, is a text cursor's.
        io.KeyRepeatRate = 0.10f;
        ImGui_ImplSDL3_InitForSDLRenderer(app.window, renderer);
        ImGui_ImplSDLRenderer3_Init(renderer);
        const ui::Fonts fonts = ui::LoadFonts(scale);
        ui::Backdrop backdrop = ui::MakeBackdrop(renderer);
        auto pictures = std::make_unique<Pictures>();

        app.state = setup::Detect();
        sound::Load();
        app.updated = updated;
        app.pendingReport = report::Pending() && !GameRunning();
        if (then != Action::None && app.state == setup::State::Installed) app.Compile(then, device);
        while (!app.quit)
        {
            SDL_Event event;
            const bool wasRenaming = app.renaming;
            while (SDL_PollEvent(&event))
            {
                ImGui_ImplSDL3_ProcessEvent(&event);
                if (event.type == SDL_EVENT_QUIT || event.type == SDL_EVENT_WINDOW_CLOSE_REQUESTED)
                {
                    if (app.job) app.job->Cancel();
                    app.quit = true;
                }
                // A disc image dropped on the window installs from it; while
                // the update is what is asked for, the file is the update.
                // A profile's new name, typed.
                if (app.renaming) app.Type(event);
                if (event.type == SDL_EVENT_DROP_FILE && event.drop.data)
                    app.Take(app.needsUpdateFile ? Action::ChooseUpdate : Action::Install, event.drop.data);
            }
            app.Poll();
            // Another start of the launcher asked for this one (single.h).
            if (single::Woken())
            {
                // A window minimized on X11 is a hidden one to SDL, which restores only what it shows.
                if (SDL_GetWindowFlags(app.window) & SDL_WINDOW_MINIMIZED)
                {
                    SDL_ShowWindow(app.window);
                    SDL_RestoreWindow(app.window);
                }
                SDL_RaiseWindow(app.window);
            }
            // The key that ended the typing is not also a choice.
            const bool typed = wasRenaming && !app.renaming;

            ImGui_ImplSDLRenderer3_NewFrame();
            ImGui_ImplSDL3_NewFrame();
            ImGui::NewFrame();

            const std::vector<Entry> entries = app.Entries();
            const int count = int(entries.size());
            const bool placed = app.focusPlaced;
            if (!app.focusPlaced)
            {
                // The first thing a player can do.
                app.focus = 0;
                for (int i = 0; i < count; i++)
                    if (entries[i].shown.enabled) { app.focus = i; break; }
                app.focusPlaced = true;
            }
            const int focusBefore = app.focus;
            auto pressed = [](std::initializer_list<ImGuiKey> keys, bool repeat) {
                for (ImGuiKey key : keys)
                    if (ImGui::IsKeyPressed(key, repeat)) return true;
                return false;
            };
            if (pressed({ ImGuiKey_DownArrow, ImGuiKey_GamepadDpadDown, ImGuiKey_GamepadLStickDown }, true)) app.focus = (app.focus + 1) % count;
            if (pressed({ ImGuiKey_UpArrow, ImGuiKey_GamepadDpadUp, ImGuiKey_GamepadLStickUp }, true)) app.focus = (app.focus + count - 1) % count;
            app.focus = std::clamp(app.focus, 0, count - 1);
            int chosen = pressed({ ImGuiKey_Enter, ImGuiKey_KeypadEnter, ImGuiKey_Space, ImGuiKey_GamepadFaceDown }, false) ? app.focus : -1;
            // While a name is typed the keys are its letters, and the entry stays.
            if (app.renaming || typed) { app.focus = focusBefore; chosen = -1; }
            if (app.renaming || typed) {}
            else if (app.JobShown() && pressed({ ImGuiKey_Escape, ImGuiKey_GamepadFaceRight }, false)) app.job->Cancel();
            else if ((app.profileScreen || app.graphicsScreen || app.mapsScreen || (app.reportScreen && !app.recording)) && pressed({ ImGuiKey_Escape, ImGuiKey_GamepadFaceRight }, false))
            {
                sound::Play(sound::Clip::Click);
                app.Do(app.mapsWarning ? Action::CustomMapsCancel : app.mapRemoving ? Action::RemoveMapNo : Action::Back);
                ImGui::EndFrame();
                continue;
            }

            ui::Frame frame;
            for (const Entry& entry : entries) frame.entries.push_back(entry.shown);
            frame.focus = app.focus;
            const Entry& current = entries[app.focus];
            frame.heading = current.heading;
            if (app.JobShown() && app.job->kind == Job::Kind::Compile)
            {
                // The game's files read, then their shaders compiled.
                const precompile::Progress& progress = app.job->compile;
                const uint32_t shaders = progress.shaders, compiled = progress.compiled;
                const uint32_t files = progress.files, read = progress.filesRead;
                frame.busy = true;
                frame.steps = 2;
                frame.step = shaders ? 2 : 1;
                frame.stepTitle = shaders ? "Compiling the shaders" : "Reading the game's files";
                frame.amount = shaders ? std::to_string(compiled) + " of " + std::to_string(shaders)
                                       : std::to_string(read) + " of " + std::to_string(files);
                frame.fraction = shaders ? float(compiled) / float(shaders) : files ? float(read) / float(files) : -1.0f;
                frame.text = progress.cancel ? "Stopping..."
                                             : progress.why + "\n\nEvery shader of the game is compiled for this computer before it is played, "
                                               "so that none is compiled in play, where it would stutter. It is done again "
                                               "when the game, the graphics driver, the graphics card, the resolution or "
                                               "the game's files change.";
            }
            else if (app.JobShown())
            {
                std::lock_guard lock(app.job->progress.lock);
                const setup::Progress& progress = app.job->progress;
                frame.busy = progress.steps > 0;
                frame.step = progress.step;
                frame.steps = progress.steps;
                frame.stepTitle = progress.title;
                frame.detail = progress.detail;
                frame.amount = Amount(progress.done, progress.total);
                frame.fraction = progress.total ? float(double(progress.done) / double(progress.total)) : -1.0f;
                frame.text = progress.cancel                            ? "Stopping..."
                             : app.job->kind == Job::Kind::CheckUpdate ? "Looking for a newer version..."
                             : app.job->kind == Job::Kind::InstallUpdate ? "The launcher starts again when it is done."
                             : app.job->kind == Job::Kind::AddMap
                                 ? "The map was made for the PC game: it is converted for the Xbox 360, with what it needs "
                                   "from the game's own files. It takes a minute or so."
                                 : "Stopping and starting again later carries on where it left off.";
            }
            else if (app.reportScreen ? !app.message.empty() && !app.recording && (app.messageFocus < 0 || app.focus == app.messageFocus)
                     : app.profileScreen || app.mapsScreen ? !app.message.empty() && (app.messageFocus < 0 || app.focus == app.messageFocus)
                                       : !app.message.empty() && (current.action == Action::Install || current.action == Action::ChooseUpdate ||
                                                                  current.action == Action::CheckUpdate || current.action == Action::InstallUpdate || app.messageIsError))
            {
                frame.text = app.message;
                frame.error = app.messageIsError;
            }
            else frame.text = current.text;
            frame.status = app.state == setup::State::Installed     ? "The game is installed."
                           : app.state == setup::State::NeedsUpdate ? "The game is installed from the disc and needs title update 6."
                                                                    : "The game is not installed.";
            frame.corner = setup::UsesUpdate() ? "TITLE UPDATE 6" : "DISC VERSION 1.0.557";
            if (*update::Current()) frame.corner = std::string(update::Current()) + "   " + frame.corner;
            if (!app.updated.empty()) frame.status = "Updated to " + app.updated + ". " + frame.status;
            frame.hint = app.recording                          ? ""
                         : app.profileScreen || app.reportScreen || app.graphicsScreen || app.mapsScreen ? "ENTER OR (A) TO CHOOSE, ESC OR (B) TO GO BACK"
                                                                 : "ENTER OR (A) TO CHOOSE";

            if (current.action == Action::NextMap && app.mapList.size() > 1)
            {
                // Left and right go through the maps, as a choice goes to the next.
                const size_t count = app.mapList.size();
                if (pressed({ ImGuiKey_LeftArrow, ImGuiKey_GamepadDpadLeft, ImGuiKey_GamepadLStickLeft }, true))
                {
                    app.mapIndex = (app.mapIndex + count - 1) % count;
                    app.message.clear();
                    sound::Play(sound::Clip::Over);
                }
                if (pressed({ ImGuiKey_RightArrow, ImGuiKey_GamepadDpadRight, ImGuiKey_GamepadLStickRight }, true))
                {
                    app.mapIndex = (app.mapIndex + 1) % count;
                    app.message.clear();
                    sound::Play(sound::Clip::Over);
                }
                frame.hint = "LEFT AND RIGHT FOR ANOTHER MAP, ESC OR (B) TO GO BACK";
            }
            frame.picture = pictures->Get(renderer, current.picture);

            if (current.action == Action::Fov)
            {
                const int range = settings::kWidestFov - settings::kConsoleFov;
                frame.slider = float(settings::Fov() - settings::kConsoleFov) / float(range);
                frame.fov = float(settings::Fov());
                frame.aspect = settings::AspectRatio() == settings::Ratio::Wider ? 16.0f / 10.0f : 16.0f / 9.0f;
                frame.hint = "LEFT AND RIGHT TO CHANGE, ESC OR (B) TO GO BACK";
            }
            // RANK and PRESTIGE: the same, over what the player's stats can hold.
            const bool rankRow = current.action == Action::Rank || current.action == Action::Prestige;
            const int rankLow = current.action == Action::Rank ? 1 : 0;
            const int rankHigh = current.action == Action::Rank ? profile::MaxLevel() : profile::MaxPrestige();
            if (rankRow)
            {
                const profile::Player& p = app.players[app.player];
                const int value = current.action == Action::Rank ? app.ShownLevel(p) : app.ShownPrestige(p);
                frame.slider = float(value - rankLow) / float(rankHigh - rankLow);
                frame.hint = "LEFT AND RIGHT TO CHANGE, ESC OR (B) TO GO BACK";
            }

            float slid = -1;
            const int pointed = ui::Draw(fonts, frame, scale, app.focus, slid);
            if (current.action == Action::Fov)
            {
                // The slider: a step a press to either side, or wherever the pointer holds it.
                int fov = settings::Fov();
                if (pressed({ ImGuiKey_LeftArrow, ImGuiKey_GamepadDpadLeft, ImGuiKey_GamepadLStickLeft }, true)) fov -= settings::kFovStep;
                if (pressed({ ImGuiKey_RightArrow, ImGuiKey_GamepadDpadRight, ImGuiKey_GamepadLStickRight }, true)) fov += settings::kFovStep;
                if (slid >= 0)
                    fov = settings::kConsoleFov + int(std::lround(slid * float(settings::kWidestFov - settings::kConsoleFov) / settings::kFovStep)) * settings::kFovStep;
                fov = std::clamp(fov, settings::kConsoleFov, settings::kWidestFov);
                if (fov != settings::Fov())
                {
                    settings::SetFov(fov);
                    sound::Play(sound::Clip::Over);
                }
            }
            if (rankRow)
            {
                const profile::Player& p = app.players[app.player];
                const bool rank = current.action == Action::Rank;
                const int was = rank ? app.ShownLevel(p) : app.ShownPrestige(p);
                int value = was;
                if (pressed({ ImGuiKey_LeftArrow, ImGuiKey_GamepadDpadLeft, ImGuiKey_GamepadLStickLeft }, true)) value--;
                if (pressed({ ImGuiKey_RightArrow, ImGuiKey_GamepadDpadRight, ImGuiKey_GamepadLStickRight }, true)) value++;
                if (slid >= 0) value = rankLow + int(std::lround(slid * float(rankHigh - rankLow)));
                value = std::clamp(value, rankLow, rankHigh);
                if (value != was)
                {
                    App::Moved& to = app.moved[p.file];
                    (rank ? to.level : to.prestige) = value;
                    app.message.clear();
                    sound::Play(sound::Clip::Over);
                }
            }
            if (pointed >= 0) chosen = pointed;
            // The game's menus tick as another entry is reached and sound a
            // choice; a screen that has just come up has reached nothing.
            if (app.focus != focusBefore && placed) sound::Play(sound::Clip::Over);
            if (chosen >= 0 && entries[chosen].shown.enabled) sound::Play(sound::Clip::Click);
            // Reading a message dismisses it: the next move shows the entries' own text again.
            if (chosen >= 0 && !app.messageIsError) app.message.clear();
            if (chosen >= 0 && chosen != app.focus) app.focus = chosen;
            if (chosen >= 0) app.Do(entries[chosen].action);

            ImGui::Render();
            SDL_SetRenderDrawColor(renderer, 40, 40, 38, 255);
            SDL_RenderClear(renderer);
            ui::DrawBackdrop(renderer, backdrop, double(SDL_GetTicks()) / 1000.0);
            ImGui_ImplSDLRenderer3_RenderDrawData(ImGui::GetDrawData(), renderer);
            SDL_RenderPresent(renderer);
        }
        app.KeepMoved();    // the window closed on the profile screen
        app.job.reset();    // waits for a job that was told to stop

        ImGui_ImplSDLRenderer3_Shutdown();
        ImGui_ImplSDL3_Shutdown();
        ImGui::DestroyContext();
        ui::DestroyBackdrop(backdrop);
        pictures.reset();
        SDL_DestroyRenderer(renderer);
        SDL_DestroyWindow(app.window);
        SDL_Quit();
        return 0;
    }
}

int main(int argc, char** argv)
{
    // The command line as paths. Windows hands main() the arguments in the
    // system's code page, which loses what it cannot spell; the wide command
    // line has them whole.
    std::vector<fs::path> arguments;
#ifdef _WIN32
    {
        int count = 0;
        if (wchar_t** wide = CommandLineToArgvW(GetCommandLineW(), &count))
        {
            for (int i = 1; i < count; i++) arguments.emplace_back(wide[i]);
            LocalFree(wide);
        }
        // A player's build has no console of its own; one it was started from
        // gets what the terminal mode prints.
        if (!arguments.empty() && AttachConsole(ATTACH_PARENT_PROCESS))
        {
            std::freopen("CONOUT$", "w", stdout);
            std::freopen("CONOUT$", "w", stderr);
        }
    }
#else
    for (int i = 1; i < argc; i++) arguments.push_back(FromUtf8(argv[i]));
#endif

    // The paths on the command line are the terminal's; everything a player's
    // copy keeps -- game/, saves/ -- is beside the launcher.
    std::error_code ec;
    fs::path disc, update;
    const bool upgrade = arguments.size() == 1 && arguments[0] == "--upgrade";
    const bool compile = arguments.size() == 1 && arguments[0] == "--compile-shaders";
    const bool addMap = arguments.size() >= 2 && arguments[0] == "--add-map";
    std::vector<fs::path> mapFiles;
    if (addMap)
        for (size_t i = 1; i < arguments.size(); i++) mapFiles.push_back(fs::absolute(arguments[i], ec));
    // --play <title> [--device <id>]: from the game, whose shaders need compiling.
    Action play = Action::None;
    std::string device;
    if (arguments.size() >= 2 && arguments[0] == "--play")
    {
        play = arguments[1] == "campaign" ? Action::PlayCampaign : arguments[1] == "multiplayer" ? Action::PlayMultiplayer : Action::None;
        if (arguments.size() == 4 && arguments[2] == "--device") device = Utf8(arguments[3]);
        else if (arguments.size() != 2) play = Action::None;
    }
    bool usage = !arguments.empty() && arguments[0] != "--install" && !upgrade && !compile && !addMap && play == Action::None;
    for (size_t i = 1; i < arguments.size() && !usage && play == Action::None && !addMap; i++)
    {
        const std::string text = Utf8(arguments[i]);
        if (text == "--update" && i + 1 < arguments.size()) update = fs::absolute(arguments[++i], ec);
        else if (text[0] != '-' && disc.empty()) disc = fs::absolute(arguments[i], ec);
        else usage = true;
    }
    if (const char* base = SDL_GetBasePath()) fs::current_path(FromUtf8(base), ec);

    // What the last update left behind goes, now that the launcher it
    // replaced has ended.
    const std::string updated = update::Finish();
    settings::Load();
    if (!arguments.empty() && play == Action::None)
    {
        if (usage)
        {
            std::fprintf(stderr, "usage: %s --install [<disc image or extracted disc folder>] [--update <package or folder>]\n"
                                 "       %s --upgrade\n"
                                 "       %s --compile-shaders\n"
                                 "       %s --add-map <one of a map's files>...\n"
                                 "       %s --play campaign|multiplayer [--device <id>]\n", argv[0], argv[0], argv[0], argv[0], argv[0]);
            return 2;
        }
        if (upgrade) return UpgradeInTerminal();
        if (compile) return CompileInTerminal();
        if (addMap) return AddMapInTerminal(mapFiles);
        return InstallInTerminal(disc, update);
    }
    // One window a copy (single.h): a second start brings the first up and
    // ends. The one an update started waits for the old one to end; one the
    // game started for its shaders does that work whatever else is open.
    const bool takeOver = std::getenv(kTakeOver) != nullptr;
    SDL_unsetenv_unsafe(kTakeOver);     // not for what this one starts
    if (!single::Claim(takeOver) && play == Action::None && !takeOver)
    {
        single::Wake();
        return 0;
    }
    return Window(updated, play, device);
}
