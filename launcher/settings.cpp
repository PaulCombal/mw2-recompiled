#include "settings.h"
#include "../runtime/render_size.h"

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <map>
#include <optional>
#include <string>
#include <utility>

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

bool settings::Fullscreen()
{
    const auto kept = g_kept.find("MW2_FULLSCREEN");
    return kept != g_kept.end() && !kept->second.empty() && kept->second != "0";
}

void settings::SetFullscreen(bool on)
{
    g_kept["MW2_FULLSCREEN"] = on ? "1" : "0";
    Save();
}

int settings::Scale()
{
    const auto kept = g_kept.find("MW2_SCALE");
    return std::clamp(kept == g_kept.end() ? 1 : std::atoi(kept->second.c_str()), 1, kMaxScale);
}

namespace
{
    // Every row is a frame the game takes.
    constexpr bool RowsValid()
    {
        for (int ratio = 0; ratio < settings::kRatios; ratio++)
            for (const auto& row : settings::Resolutions(settings::Ratio(ratio)))
                if (!render::FrameValid({ row.width, row.height })) return false;
        return true;
    }
    static_assert(RowsValid());

    // MW2_RENDER's frame, the console's when there is none or it is no size.
    std::pair<unsigned, unsigned> KeptFrame()
    {
        const auto kept = g_kept.find("MW2_RENDER");
        unsigned width = 0, height = 0;
        if (kept == g_kept.end() || std::sscanf(kept->second.c_str(), "%ux%u", &width, &height) != 2 || !height)
            return { 1280, 720 };
        return { width, height };
    }
}

const char* settings::RatioName(Ratio ratio)
{
    return ratio == Ratio::Wider ? "16:10" : ratio == Ratio::Standard ? "4:3" : "16:9";
}

float settings::Aspect(Ratio ratio)
{
    return ratio == Ratio::Wider ? 16.0f / 10.0f : ratio == Ratio::Standard ? 4.0f / 3.0f : 16.0f / 9.0f;
}

settings::Ratio settings::AspectRatio()
{
    const auto [width, height] = KeptFrame();
    if (width * 10 == height * 16) return Ratio::Wider;
    if (width * 3 == height * 4) return Ratio::Standard;
    return Ratio::Wide;
}

int settings::ResolutionRow()
{
    const auto [width, height] = KeptFrame();
    const auto rows = Resolutions(AspectRatio());
    for (size_t row = 0; row < rows.size(); row++)
        if (rows[row].width == width && rows[row].height == height && rows[row].scale == Scale()) return int(row);
    for (size_t row = 0; row < rows.size(); row++)
        if (rows[row].scale == Scale()) return int(row);
    return 0;
}

const settings::Resolution& settings::CurrentResolution()
{
    return Resolutions(AspectRatio())[ResolutionRow()];
}

void settings::NextAspectRatio()
{
    const Resolution& kept = CurrentResolution();
    const unsigned height = kept.height * unsigned(kept.scale);
    const Ratio next = Ratio((int(AspectRatio()) + 1) % kRatios);
    const auto rows = Resolutions(next);
    int nearest = 0;
    for (size_t row = 1; row < rows.size(); row++)
    {
        auto off = [&](size_t at) { return std::abs(int(rows[at].height) * rows[at].scale - int(height)); };
        if (off(row) < off(nearest)) nearest = int(row);
    }
    SetResolution(next, nearest);
}

void settings::SetResolution(Ratio ratio, int row)
{
    const auto rows = Resolutions(ratio);
    const Resolution& chosen = rows[std::clamp(row, 0, int(rows.size()) - 1)];
    if (chosen.width == 1280 && chosen.height == 720) g_kept.erase("MW2_RENDER");
    else g_kept["MW2_RENDER"] = std::to_string(chosen.width) + "x" + std::to_string(chosen.height);
    g_kept["MW2_SCALE"] = std::to_string(chosen.scale);
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

int settings::Msaa()
{
    const auto off = g_kept.find("MW2_NO_MSAA");
    if (off != g_kept.end() && !off->second.empty() && off->second != "0") return 0;
    const auto kept = g_kept.find("MW2_MSAA");
    const int samples = kept == g_kept.end() ? 0 : std::atoi(kept->second.c_str());
    return samples > 1 ? samples : 2;
}

void settings::SetMsaa(int samples)
{
    g_kept.erase("MW2_NO_MSAA");
    g_kept.erase("MW2_MSAA");
    if (samples == 0) g_kept["MW2_NO_MSAA"] = "1";
    else if (samples > 2) g_kept["MW2_MSAA"] = std::to_string(samples);
    Save();
}

bool settings::CustomMaps()
{
    const auto kept = g_kept.find("MW2_CUSTOM_MAPS");
    return kept != g_kept.end() && !kept->second.empty() && kept->second != "0";
}

void settings::SetCustomMaps(bool on)
{
    // Off is the game's own way: the line goes.
    if (on) g_kept["MW2_CUSTOM_MAPS"] = "1";
    else g_kept.erase("MW2_CUSTOM_MAPS");
    Save();
}

bool settings::Sounds()
{
    const auto kept = g_kept.find("MW2_LAUNCHER_SOUNDS");
    return kept == g_kept.end() || kept->second != "0";
}

std::optional<std::string> settings::PcGame()
{
    const auto kept = g_kept.find("MW2_PC_GAME");
    return kept == g_kept.end() ? std::nullopt : std::optional(kept->second);
}

void settings::SetPcGame(const std::string& folder)
{
    g_kept["MW2_PC_GAME"] = folder;
    Save();
}
