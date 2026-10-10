#include "settings.h"

#include <algorithm>
#include <cstdlib>
#include <fstream>
#include <map>
#include <string>

namespace
{
    constexpr const char* kFile = ".env";
    // Every line, so the ones a player wrote by hand are written back.
    std::map<std::string, std::string> g_kept;

    void Save()
    {
        std::ofstream file(kFile, std::ios::trunc);
        for (const auto& [name, value] : g_kept) file << name << '=' << value << '\n';
    }
}

void settings::Load()
{
    std::ifstream file(kFile);
    for (std::string line; std::getline(file, line);)
    {
        while (!line.empty() && (line.back() == '\r' || line.back() == ' ')) line.pop_back();
        const size_t equals = line.find('=');
        if (equals == std::string::npos) continue;
        const std::string name = line.substr(0, equals), value = line.substr(equals + 1);
        g_kept[name] = value;
    }
}

int settings::Scale()
{
    const auto kept = g_kept.find("MW2_SCALE");
    return std::clamp(kept == g_kept.end() ? 1 : std::atoi(kept->second.c_str()), 1, kMaxScale);
}

void settings::SetScale(int scale)
{
    g_kept["MW2_SCALE"] = std::to_string(std::clamp(scale, 1, kMaxScale));
    Save();
}

bool settings::HasFpsLimit() { return g_kept.count("MW2_FPS_LIMIT") != 0; }

int settings::FpsLimit()
{
    const auto kept = g_kept.find("MW2_FPS_LIMIT");
    return kept == g_kept.end() ? kConsoleFps : std::clamp(std::atoi(kept->second.c_str()), 0, 1000);
}

void settings::SetFpsLimit(int limit)
{
    g_kept["MW2_FPS_LIMIT"] = std::to_string(limit);
    Save();
}

int settings::Fov()
{
    const auto kept = g_kept.find("MW2_FOV");
    return kept == g_kept.end() ? kConsoleFov : std::clamp(std::atoi(kept->second.c_str()), kConsoleFov, kWidestFov);
}

void settings::SetFov(int fov)
{
    g_kept["MW2_FOV"] = std::to_string(std::clamp(fov, kConsoleFov, kWidestFov));
    Save();
}

bool settings::Sounds()
{
    const auto kept = g_kept.find("MW2_LAUNCHER_SOUNDS");
    return kept == g_kept.end() || kept->second != "0";
}
