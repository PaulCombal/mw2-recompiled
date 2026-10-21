#pragma once
// A custom map's .arena: what the multiplayer's menus show of it, in the PC
// game's format, { key "value" ... } a map. The game lists the maps in it
// (runtime/custom_map_list.cpp) and the launcher's MAPS screen shows them.
#include <algorithm>
#include <cctype>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <string>
#include <system_error>
#include <vector>

namespace arena
{
    struct Map
    {
        std::string map, longname, description, mapimage;
    };

    // The .arena files in `game` whose map is installed: <name>.ff and
    // <name>_load.ff beside it.
    inline std::vector<std::filesystem::path> Installed(const std::filesystem::path& game)
    {
        namespace fs = std::filesystem;
        std::vector<fs::path> arenas;
        std::error_code ec;
        for (const auto& entry : fs::directory_iterator(game, ec))
        {
            const fs::path& path = entry.path();
            if (path.extension() != ".arena") continue;
            const fs::path stem = path.stem();
            fs::path map = path, load = path;
            map.replace_filename(stem.native() + fs::path(".ff").native());
            load.replace_filename(stem.native() + fs::path("_load.ff").native());
            if (fs::is_regular_file(map, ec) && fs::is_regular_file(load, ec)) arenas.push_back(path);
        }
        std::sort(arenas.begin(), arenas.end());
        return arenas;
    }

    inline std::string Read(const std::filesystem::path& path)
    {
        std::ifstream file(path, std::ios::binary);
        return std::string((std::istreambuf_iterator<char>(file)), std::istreambuf_iterator<char>());
    }

    // The maps an arena's text describes.
    inline std::vector<Map> Maps(const std::string& text)
    {
        std::vector<Map> maps;
        Map map;
        std::string key;
        for (size_t i = 0; i < text.size(); i++)
        {
            const char c = text[i];
            if (c == '{') { map = {}; key.clear(); }
            else if (c == '}') { if (!map.map.empty()) maps.push_back(map); }
            else if (c == '"')
            {
                const size_t end = text.find('"', i + 1);
                if (end == std::string::npos) break;
                const std::string value = text.substr(i + 1, end - i - 1);
                i = end;
                if (key.empty()) { key = value; continue; }
                if (key == "map") map.map = value;
                else if (key == "longname") map.longname = value;
                else if (key == "description") map.description = value;
                else if (key == "mapimage") map.mapimage = value;
                key.clear();
            }
            else if (std::isalpha(static_cast<unsigned char>(c)) || c == '_')
            {
                size_t end = i;
                while (end < text.size() && (std::isalnum(static_cast<unsigned char>(text[end])) || text[end] == '_')) end++;
                key = text.substr(i, end - i);
                i = end - 1;
            }
        }
        return maps;
    }
}
