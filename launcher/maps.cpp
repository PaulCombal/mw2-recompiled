#include "maps.h"
#include "disc.h"
#include "settings.h"
#include "setup.h"
#include "mapport/fastfile.h"
#include "mapport/images.h"
#include "mapport/port.h"
#include "mapport/query.h"
#include "mapport/zip.h"
#include "../runtime/install/arena.h"

#include <SDL3/SDL.h>

#ifdef _WIN32
#include <windows.h>
#endif

#include <algorithm>
#include <cstdint>
#include <cstring>
#include <fstream>
#include <iterator>
#include <map>
#include <optional>
#include <set>
#include <string_view>

namespace fs = std::filesystem;

namespace
{
    const fs::path kStore = "maps";   // beside the launcher: the maps not in the game, and every picture

    // The picture in the pane is 416x234 at the design's size.
    constexpr int kPictureWidth = 416, kPictureHeight = 234;

    std::string Utf8(const fs::path& path)
    {
        const std::u8string text = path.u8string();
        return std::string(text.begin(), text.end());
    }

    fs::path Named(const fs::path& folder, const std::string& name, std::string_view suffix)
    {
        const std::string text = name + std::string(suffix);
        return folder / fs::path(std::u8string(text.begin(), text.end()));
    }

    std::vector<uint8_t> ReadFile(const fs::path& path, size_t most = SIZE_MAX)
    {
        std::ifstream file(path, std::ios::binary);
        std::vector<uint8_t> bytes;
        if (!file) return bytes;
        if (most == SIZE_MAX) bytes.assign(std::istreambuf_iterator<char>(file), {});
        else
        {
            bytes.resize(most);
            file.read(reinterpret_cast<char*>(bytes.data()), std::streamsize(most));
            bytes.resize(size_t(file.gcount()));
        }
        return bytes;
    }

    // ---- the picture: a .iwd is a zip of the PC game's .iwi images ----------

    // The .iwd's picture of that name (as the game looks it up, whatever the
    // case or the slashes), decoded to RGBA, the largest level.
    bool ReadPicture(const mapport::Zip& iwd, const std::string& image, int& width, int& height, std::vector<uint8_t>& rgba)
    {
        const std::string wanted = "images/" + image + ".iwi";
        for (size_t k = 0; k < iwd.Names().size(); k++)
        {
            std::string name = iwd.Names()[k];
            std::replace(name.begin(), name.end(), '\\', '/');
            if (mapport::Lower(name) != mapport::Lower(wanted)) continue;
            const mapport::Picture pic = mapport::ReadIwi(iwd.Read(k));
            if (pic.cube || pic.mips.empty()) return false;
            width = pic.width;
            height = pic.height;
            rgba = mapport::DecodePixels(pic.mips[0], width, height, pic.format);
            return true;
        }
        return false;
    }

    // The map's picture from its .iwd, the size the pane shows it, as the game
    // stretches a loading screen over the screen. Its mapimage when the .iwd
    // has it, else its loading screen.
    bool MakePicture(const fs::path& iwd, const arena::Map& map, const std::string& name, const fs::path& to)
    {
        int width = 0, height = 0;
        std::vector<uint8_t> pixels;
        try
        {
            const mapport::Zip zip(iwd);
            bool found = false;
            for (const std::string& image : { map.mapimage, "loadscreen_" + name, "loadscreen_" + map.map })
                if (!image.empty() && (found = ReadPicture(zip, image, width, height, pixels))) break;
            if (!found || width < 1 || height < 1 || pixels.size() < size_t(width) * size_t(height) * 4) return false;
        }
        catch (const std::exception&)   // (a broken file can also ask for more memory than there is)
        {
            return false;
        }
        std::vector<uint8_t> shown(size_t(kPictureWidth) * kPictureHeight * 4);
        for (int y = 0; y < kPictureHeight; y++)
            for (int x = 0; x < kPictureWidth; x++)
            {
                const size_t from = (size_t(y * height / kPictureHeight) * size_t(width) + size_t(x * width / kPictureWidth)) * 4;
                uint8_t* p = &shown[(size_t(y) * kPictureWidth + size_t(x)) * 4];
                std::memcpy(p, &pixels[from], 3);
                p[3] = 255;
            }
        SDL_Surface* surface = SDL_CreateSurfaceFrom(kPictureWidth, kPictureHeight, SDL_PIXELFORMAT_RGBA32, shown.data(), kPictureWidth * 4);
        if (!surface) return false;
        const bool saved = SDL_SaveBMP(surface, Utf8(to).c_str());
        SDL_DestroySurface(surface);
        return saved;
    }

    // ---- the files --------------------------------------------------------

    // A fastfile the 360's multiplayer loads: unsigned, as nobody but the
    // game's makers can sign one, and of the version of the game's own.
    bool ConsoleFastfile(const fs::path& file, std::string& why)
    {
        const std::vector<uint8_t> head = ReadFile(file, 12), own = ReadFile(setup::GameFolder() / "common_mp.ff", 12);
        const std::string name = Utf8(file.filename());
        if (head.size() < 12) { why = name + " could not be read."; return false; }
        if (std::memcmp(head.data(), "IWffu100", 8))
        {
            why = name + " is not a map file of the game.";
            return false;
        }
        if (own.size() == 12 && std::memcmp(head.data() + 8, own.data() + 8, 4))
        {
            why = name + " was made for another version of the game.";
            return false;
        }
        return true;
    }

    bool Move(const fs::path& from, const fs::path& to)
    {
        std::error_code ec;
        fs::rename(from, to, ec);
        if (!ec) return true;
        // Another drive: a copy, then the original goes.
        fs::copy_file(from, to, fs::copy_options::overwrite_existing, ec);
        if (ec) return false;
        fs::remove(from, ec);
        return true;
    }

    // A map's files in the game. The last, the menus' picture of the map, a
    // map can be without: one converted before maps came with it, or one made
    // for the 360.
    const char* const kSuffixes[] = { ".ff", "_load.ff", ".arena", "_preview.ff" };
    constexpr std::string_view kPreview = "_preview.ff";

    // The arena's stem: the name the files share.
    std::string Stem(const fs::path& arena) { return Utf8(arena.stem()); }

    // ---- converting a map made for the PC game ------------------------------

    bool PcFastfile(const fs::path& file)
    {
        try
        {
            return mapport::IsPcFastfile(file);
        }
        catch (const mapport::Error&)
        {
            return false;
        }
    }

    // The game's own files, which a conversion takes what the map needs from:
    // the game folder's fastfiles, but for the custom maps installed there.
    std::vector<fs::path> StockFiles()
    {
        std::set<fs::path> custom;
        for (const fs::path& arena : arena::Installed(setup::GameFolder()))
            for (const char* suffix : { ".ff", "_load.ff", "_preview.ff" }) custom.insert(Named(setup::GameFolder(), Stem(arena), suffix));
        std::vector<fs::path> stock;
        std::error_code ec;
        for (const auto& entry : fs::directory_iterator(setup::GameFolder(), ec))
            if (entry.path().extension() == ".ff" && !custom.count(entry.path())) stock.push_back(entry.path());
        std::sort(stock.begin(), stock.end());
        return stock;
    }

    // ---- the PC game: the pictures and sounds a map takes from it ----------

    // An environment variable naming a folder. (On Windows it is read wide:
    // the narrow one is in the ANSI code page.)
    std::optional<fs::path> FolderVariable(const char* name)
    {
#ifdef _WIN32
        const std::wstring wide(name, name + std::strlen(name));
        if (const wchar_t* value = _wgetenv(wide.c_str())) return fs::path(value);
#else
        if (const char* value = std::getenv(name)) return install::FromUtf8(value);
#endif
        return std::nullopt;
    }

    // Whether a folder is the PC game's: its main/ (or the folder itself, its
    // main folder chosen) has the game's first .iwd.
    bool HoldsPcGame(const fs::path& folder)
    {
        std::error_code ec;
        return fs::is_regular_file(folder / "main" / "iw_00.iwd", ec) || fs::is_regular_file(folder / "iw_00.iwd", ec);
    }

    // The places Steam is installed in: on Linux, the usual ones, the snap's
    // and the Flatpak's; on Windows, where the registry says, else the usual
    // one. Each once.
    std::vector<fs::path> SteamFolders()
    {
        std::vector<fs::path> candidates;
#ifdef _WIN32
        const struct
        {
            HKEY root;
            const wchar_t *key, *value;
        } kKeys[] = {
            { HKEY_CURRENT_USER, L"Software\\Valve\\Steam", L"SteamPath" },
            { HKEY_LOCAL_MACHINE, L"SOFTWARE\\WOW6432Node\\Valve\\Steam", L"InstallPath" },
            { HKEY_LOCAL_MACHINE, L"SOFTWARE\\Valve\\Steam", L"InstallPath" },
        };
        for (const auto& [root, key, value] : kKeys)
        {
            wchar_t folder[MAX_PATH];
            DWORD bytes = sizeof(folder);
            if (RegGetValueW(root, key, value, RRF_RT_REG_SZ, nullptr, folder, &bytes) == ERROR_SUCCESS)
                candidates.emplace_back(folder);
        }
        if (const std::optional<fs::path> programs = FolderVariable("ProgramFiles(x86)")) candidates.push_back(*programs / "Steam");
#else
        // (Started from a snap, HOME is the snap's and the player's own is SNAP_REAL_HOME.)
        for (const char* variable : { "HOME", "SNAP_REAL_HOME" })
            if (const std::optional<fs::path> home = FolderVariable(variable))
                for (const char* where : { ".local/share/Steam", ".steam/steam", ".steam/root", ".steam/debian-installation",
                                           "snap/steam/common/.local/share/Steam",
                                           ".var/app/com.valvesoftware.Steam/.local/share/Steam" })
                    candidates.push_back(*home / where);
#endif
        std::vector<fs::path> found;
        std::set<fs::path> seen;
        std::error_code ec;
        for (const fs::path& folder : candidates)
            if (fs::is_directory(folder, ec) && seen.insert(fs::weakly_canonical(folder, ec)).second) found.push_back(folder);
        return found;
    }

    // The values of `key` in a Steam .vdf file ("key"  "value", a pair a
    // line; \\ and \" stand for the character after the backslash).
    std::vector<std::string> VdfValues(const fs::path& file, const std::string& key)
    {
        std::vector<std::string> values;
        std::ifstream vdf(file);
        for (std::string line; std::getline(vdf, line);)
        {
            std::vector<std::string> quoted;
            for (size_t i = 0; i < line.size(); i++)
            {
                if (line[i] != '"') continue;
                std::string text;
                for (i++; i < line.size() && line[i] != '"'; i++)
                {
                    if (line[i] == '\\' && i + 1 < line.size()) i++;
                    text += line[i];
                }
                quoted.push_back(std::move(text));
            }
            if (quoted.size() == 2 && quoted[0] == key) values.push_back(std::move(quoted[1]));
        }
        return values;
    }

    // Steam's apps of the PC game: the campaign's and the multiplayer's, which
    // share one folder.
    constexpr const char* kPcGameApps[] = { "10190", "10180" };
    constexpr const char* kPcGameFolder = "Call of Duty Modern Warfare 2";

    // The PC game in a Steam library, or empty: the folder its app manifest
    // names, when the game is there.
    fs::path PcGameIn(const fs::path& library)
    {
        std::vector<std::string> folders;
        for (const char* app : kPcGameApps)
            for (std::string& dir : VdfValues(library / "steamapps" / ("appmanifest_" + std::string(app) + ".acf"), "installdir"))
                folders.push_back(std::move(dir));
        folders.push_back(kPcGameFolder);
        for (const std::string& dir : folders)
        {
            const fs::path game = library / "steamapps" / "common" / install::FromUtf8(dir);
            if (HoldsPcGame(game)) return game;
        }
        return {};
    }

    // What a log line of the conversion says to a player: without its indent
    // and its "note: ".
    std::string Shown(const std::string& line)
    {
        constexpr std::string_view kNote = "note: ";
        std::string text = line.substr(std::min(line.size(), line.find_first_not_of(' ')));
        if (text.starts_with(kNote)) text.erase(0, kNote.size());
        return text;
    }

    // Converts a PC map into `work`: <name>.ff and <name>_load.ff for the 360.
    // Its log goes to maps/<name>.log; `notes` gets what it warned of (pictures
    // missing, shader sets taken in place of others, ...).
    // `game`: the PC game's folder, or empty.
    bool Convert(const mapport::PcMap& map, const fs::path& work, const std::string& name, const fs::path& game,
                 setup::Progress* progress, std::vector<std::string>& notes, std::string& why)
    {
        std::error_code ec;
        fs::remove_all(work, ec);
        fs::create_directories(work, ec);
        std::ofstream logFile(Named(kStore, name, ".log"), std::ios::binary);
        int step = 0;
        if (progress)
        {
            std::lock_guard lock(progress->lock);
            progress->step = 1;
            progress->steps = map.load ? 2 : 1;
            progress->title = map.load ? "Converting the loading screen" : "Converting the map";
            progress->detail.clear();
            progress->done = progress->total = 0;
        }
        const mapport::Log log = [&](const std::string& line) {
            logFile << line << "\n";
            if (!progress) return;
            if (progress->cancel) throw mapport::Cancelled();
            std::lock_guard lock(progress->lock);
            // The loading screen done (or left out), the map is next.
            if (map.load && step == 0 && line == "converting " + Utf8(map.ff.filename()))
            {
                step = 1;
                progress->step = 2;
                progress->title = "Converting the map";
            }
            progress->detail = Shown(line);
        };
        try
        {
            mapport::PortOptions options;
            if (!game.empty())
            {
                options.gameIwds = mapport::GameIwds(game);
                log(options.gameIwds.empty() ? "no .iwd files in " + Utf8(game) + ", the PC game's folder"
                                             : "the PC game's pictures and sounds: " + Utf8(game));
            }
            notes = mapport::PortMap(map, work, StockFiles(), log, options).warnings;
        }
        catch (const mapport::Cancelled&)
        {
            why = "it was stopped.";
            return false;
        }
        catch (const std::exception& e)
        {
            logFile << e.what() << "\n";
            why = std::string("it could not be converted: ") + e.what() + ".";
            return false;
        }
        // (The converter names what it writes after the .ff.)
        if (!fs::is_regular_file(Named(work, Stem(map.ff), "_load.ff"), ec))
        {
            why = "its loading screen could not be converted (" + Utf8(Named(kStore, name, ".log")) + " says why).";
            return false;
        }
        return true;
    }

    void Collect(const fs::path& folder, bool inGame, std::vector<maps::Map>& into)
    {
        for (const fs::path& file : arena::Installed(folder))
        {
            maps::Map map;
            map.name = Stem(file);
            map.inGame = inGame;
            const std::vector<arena::Map> described = arena::Maps(arena::Read(file));
            if (!described.empty())
            {
                map.longname = described[0].longname;
                map.description = described[0].description;
            }
            if (map.longname.empty()) map.longname = map.name;
            std::error_code ec;
            const fs::path picture = Named(kStore, map.name, ".bmp");
            if (fs::is_regular_file(picture, ec)) map.picture = picture;
            into.push_back(std::move(map));
        }
    }
}

std::vector<maps::Map> maps::List()
{
    std::vector<Map> list;
    Collect(setup::GameFolder(), true, list);
    Collect(kStore, false, list);
    return list;
}

size_t maps::InGame(const std::vector<Map>& list)
{
    return size_t(std::count_if(list.begin(), list.end(), [](const Map& m) { return m.inGame; }));
}

namespace
{
    // How a map's files are named after it (_load.ff and _preview.ff before .ff: they end the same).
    constexpr std::string_view kMapFiles[] = { "_load.ff", "_preview.ff", ".ff", ".arena", ".iwd" };

    // The chosen files and, beside each, the other files of its map: a map's
    // files share its name (mp_x.ff, mp_x_load.ff, mp_x.arena, mp_x.iwd), so
    // choosing one of them is enough. Names match whatever their case.
    std::vector<fs::path> WithItsFiles(const std::vector<fs::path>& chosen)
    {
        std::vector<fs::path> out = chosen;
        std::set<std::string> have;   // (folder, file name in lower case)
        auto key = [](const fs::path& file) { return Utf8(file.parent_path()) + '\0' + mapport::Lower(Utf8(file.filename())); };
        for (const fs::path& file : chosen) have.insert(key(file));
        std::set<std::pair<fs::path, std::string>> looked;   // (folder, map)
        for (const fs::path& file : chosen)
        {
            const std::string name = mapport::Lower(Utf8(file.filename()));
            const auto ending = std::find_if(std::begin(kMapFiles), std::end(kMapFiles), [&](std::string_view e) {
                return name.size() > e.size() && name.ends_with(e);
            });
            if (ending == std::end(kMapFiles)) continue;
            const std::string map = name.substr(0, name.size() - ending->size());
            const fs::path folder = file.has_parent_path() ? file.parent_path() : fs::path(".");
            if (!looked.insert({ folder, map }).second) continue;
            std::error_code ec;
            for (const auto& entry : fs::directory_iterator(folder, ec))
            {
                const std::string other = mapport::Lower(Utf8(entry.path().filename()));
                const bool ours = std::any_of(std::begin(kMapFiles), std::end(kMapFiles),
                                              [&](std::string_view e) { return other == map + std::string(e); });
                if (ours && entry.is_regular_file(ec) && have.insert(key(file.parent_path() / entry.path().filename())).second)
                    out.push_back(file.parent_path() / entry.path().filename());
            }
        }
        return out;
    }
}

bool maps::Converts(const std::vector<fs::path>& chosen)
{
    const std::vector<fs::path> files = WithItsFiles(chosen);
    return std::any_of(files.begin(), files.end(), [](const fs::path& f) { return mapport::Lower(Utf8(f.extension())) == ".ff" && PcFastfile(f); });
}

bool maps::Add(const std::vector<fs::path>& chosen, std::string& said, setup::Progress* progress, const fs::path& pcGame)
{
    const std::vector<fs::path> files = WithItsFiles(chosen);
    std::map<std::string, fs::path> byName;   // the map files by their name, lower case
    for (const fs::path& file : files)
    {
        std::string name = Utf8(file.filename());
        std::transform(name.begin(), name.end(), name.begin(), [](unsigned char c) { return char(std::tolower(c)); });
        byName[name] = file;
    }
    auto find = [&](const std::string& name) -> const fs::path* {
        std::string lower = name;
        std::transform(lower.begin(), lower.end(), lower.begin(), [](unsigned char c) { return char(std::tolower(c)); });
        const auto it = byName.find(lower);
        return it == byName.end() ? nullptr : &it->second;
    };

    std::vector<fs::path> arenas;
    for (const auto& [name, file] : byName)
        if (name.ends_with(".arena")) arenas.push_back(file);
    if (arenas.empty())
    {
        said = "No <map>.arena was found with the files chosen: a map is its <map>.ff, <map>_load.ff, <map>.arena and "
               "<map>.iwd, in one folder, as they came with it.";
        return false;
    }

    std::error_code ec;
    fs::create_directories(kStore, ec);
    std::vector<Map> installed = List();
    std::string added, waiting, failed;
    std::vector<std::string> notes;
    bool stopped = false;
    for (const fs::path& arenaFile : arenas)
    {
        const std::string name = Stem(arenaFile);
        const fs::path* ff = find(name + ".ff");
        const fs::path* load = find(name + "_load.ff");
        // The picture's, and a PC map's pictures: an .iwd of the same name, or the only one chosen.
        const fs::path* iwd = find(name + ".iwd");
        if (!iwd)
        {
            const fs::path* only = nullptr;
            int count = 0;
            for (const auto& [n, file] : byName)
                if (n.ends_with(".iwd")) { only = &file; count++; }
            if (count == 1) iwd = only;
        }
        std::string why;
        fs::path from[std::size(kSuffixes)];   // (an empty one: the map has none)
        const fs::path work = kStore / ".converting";
        if (!ff || !load) why = "its " + name + (!ff ? ".ff" : "_load.ff") + " isn't beside its .arena.";
        else if (PcFastfile(*ff) || PcFastfile(*load))
        {
            // Made for the PC game: converted for the 360 first.
            std::vector<std::string> own;
            if (!PcFastfile(*ff) || !PcFastfile(*load))
                why = "one of its fastfiles is the PC game's and the other the Xbox 360's: choose the two that came together.";
            else if (!setup::UsesUpdate())
                why = "it is made for the PC game, and only a build for title update 6 converts maps for the Xbox 360.";
            else if (Convert({ *ff, *load, iwd ? std::optional(*iwd) : std::nullopt, arenaFile }, work, name, pcGame, progress,
                             own, why))
            {
                from[0] = Named(work, Stem(*ff), ".ff");
                from[1] = Named(work, Stem(*ff), "_load.ff");
                if (const fs::path preview = Named(work, Stem(*ff), kPreview); fs::is_regular_file(preview, ec)) from[3] = preview;
                for (const std::string& n : own) notes.push_back((arenas.size() > 1 ? name + ": " : std::string()) + n);
            }
            stopped = progress && progress->cancel;
        }
        else if (ConsoleFastfile(*ff, why) && ConsoleFastfile(*load, why))
        {
            from[0] = *ff;
            from[1] = *load;
            // Its menu picture, which the game loads with its menus at every start: only one it can load.
            std::string notLoadable;
            if (const fs::path* preview = find(name + std::string(kPreview)))
            {
                if (ConsoleFastfile(*preview, notLoadable)) from[3] = *preview;
                else notes.push_back((arenas.size() > 1 ? name + ": " : std::string()) + "its menu picture is left out: " + notLoadable);
            }
        }
        if (!why.empty() || stopped)
        {
            failed += "\n" + name + ": " + (why.empty() ? std::string("it was stopped.") : why);
            fs::remove_all(work, ec);
            if (stopped) break;
            continue;
        }
        from[2] = arenaFile;

        // In the game while there is room; a map added again stays where it was.
        const auto was = std::find_if(installed.begin(), installed.end(), [&](const Map& m) { return m.name == name; });
        const bool inGame = was != installed.end() ? was->inGame : InGame(installed) < kInGame;
        const fs::path folder = inGame ? setup::GameFolder() : kStore;
        bool copied = true;
        for (size_t i = 0; i < std::size(kSuffixes) && copied; i++)
        {
            const fs::path to = Named(folder, name, kSuffixes[i]);
            if (from[i].empty())
            {
                fs::remove(to, ec);   // (one an earlier copy of the map came with)
                continue;
            }
            if (fs::equivalent(from[i], to, ec)) continue;
            // (what was there is replaced, a link too, not written through)
            fs::remove(to, ec);
            fs::copy_file(from[i], to, fs::copy_options::overwrite_existing, ec);
            // Without its menu picture the map still plays.
            copied = !ec || std::string_view(kSuffixes[i]) == kPreview;
        }
        fs::remove_all(work, ec);
        if (!copied) { failed += "\n" + name + ": its files could not be copied into " + Utf8(folder) + " (" + ec.message() + ")."; continue; }

        const std::vector<arena::Map> described = arena::Maps(arena::Read(arenaFile));
        if (iwd) MakePicture(*iwd, described.empty() ? arena::Map{} : described[0], name, Named(kStore, name, ".bmp"));

        const std::string shown = described.empty() || described[0].longname.empty() ? name : described[0].longname;
        (inGame ? added : waiting) += (inGame ? added : waiting).empty() ? shown : ", " + shown;
        installed = List();
    }
    said.clear();
    if (!added.empty()) said += "Added, and in the game: " + added + ".";
    if (!waiting.empty())
        said += (said.empty() ? "" : "\n\n") + std::string("Added: ") + waiting + ". The game already has " +
                std::to_string(kInGame) + " custom maps, so to play these, take another out of the game first.";
    if (!notes.empty())
    {
        // What the conversion noted, the first few: the rest are in the log.
        said += "\n\nThe conversion noted:";
        for (size_t i = 0; i < std::min<size_t>(3, notes.size()); i++) said += "\n- " + notes[i];
        if (notes.size() > 3) said += "\n- and " + std::to_string(notes.size() - 3) + " more, in the log in the maps folder.";
    }
    if (!failed.empty()) said += (said.empty() ? std::string("No map was added.") : std::string("\n\nNot added:")) + failed;
    return failed.empty();
}

bool maps::SetInGame(const Map& map, bool on, std::string& said)
{
    if (map.inGame == on) return true;
    if (on && InGame(List()) >= kInGame)
    {
        said = "The game has room for " + std::to_string(kInGame) + " custom maps, and has them. Take another out of the game first.";
        return false;
    }
    std::error_code ec;
    fs::create_directories(kStore, ec);
    const fs::path from = on ? kStore : setup::GameFolder(), to = on ? setup::GameFolder() : kStore;
    for (const char* suffix : kSuffixes)
        if (std::error_code missing; std::string_view(suffix) == kPreview && !fs::exists(Named(from, map.name, suffix), missing)) continue;
        else if (!Move(Named(from, map.name, suffix), Named(to, map.name, suffix)))
        {
            said = map.longname + " could not be moved: " + Utf8(Named(from, map.name, suffix)) + ".";
            return false;
        }
    said = on ? map.longname + " is in the game: in a private match, GAME SETUP, the map, then BONUS MAPS."
              : map.longname + " is out of the game. Its files wait in the maps folder beside the launcher.";
    return true;
}

bool maps::Remove(const Map& map, std::string& said)
{
    std::error_code ec;
    const fs::path folder = map.inGame ? setup::GameFolder() : kStore;
    for (const char* suffix : kSuffixes) fs::remove(Named(folder, map.name, suffix), ec);
    fs::remove(Named(kStore, map.name, ".bmp"), ec);
    fs::remove(Named(kStore, map.name, ".log"), ec);
    said = map.longname + " is removed.";
    return true;
}

fs::path maps::FindPcGame()
{
    for (const fs::path& steam : SteamFolders())
    {
        std::vector<fs::path> libraries{ steam };
        for (const std::string& path : VdfValues(steam / "steamapps" / "libraryfolders.vdf", "path"))
            libraries.push_back(install::FromUtf8(path));
        for (const fs::path& library : libraries)
            if (fs::path game = PcGameIn(library); !game.empty()) return game;
    }
    return {};
}

fs::path maps::PcGame()
{
    if (const std::optional<fs::path> set = FolderVariable("MW2_PC_GAME")) return *set;
    if (const std::optional<std::string> kept = settings::PcGame()) return install::FromUtf8(*kept);
    // Steam's folders are searched once a run: the menus ask every frame.
    static bool searched = false;
    if (searched) return {};
    searched = true;
    const fs::path found = FindPcGame();
    if (!found.empty()) settings::SetPcGame(Utf8(found));
    return found;
}

bool maps::SetPcGame(const fs::path& folder, std::string& said)
{
    if (!HoldsPcGame(folder))
    {
        said = Utf8(folder) + " isn't the PC game's folder: it has no main" + (fs::path::preferred_separator == '\\' ? "\\" : "/") +
               "iw_00.iwd. Choose the folder the PC game is installed in, the one with main in it.";
        return false;
    }
    settings::SetPcGame(Utf8(folder));
    said = "Maps added from now on take the pictures and sounds they lack from the PC game in " + Utf8(folder) + ".";
    return true;
}
