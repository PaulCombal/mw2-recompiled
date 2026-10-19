#include "shader_marker.h"
#include "shader_build.h"
#include "pipeline.h"

#include <algorithm>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <sstream>
#include <system_error>

#ifdef MW2_HAVE_VULKAN
#include <vulkan/vulkan.h>
#endif

namespace fs = std::filesystem;

namespace
{
    std::string Utf8(const fs::path& path)
    {
        const std::u8string text = path.u8string();
        return std::string(text.begin(), text.end());
    }

    std::string Hex(const uint8_t* bytes, size_t count)
    {
        std::string text;
        for (size_t i = 0; i < count; i++)
        {
            char hex[3];
            std::snprintf(hex, sizeof hex, "%02x", bytes[i]);
            text += hex;
        }
        return text;
    }

    std::vector<std::string> Lines(const std::string& text)
    {
        std::vector<std::string> lines;
        std::istringstream in(text);
        for (std::string line; std::getline(in, line);) lines.push_back(line);
        return lines;
    }

    std::string Key(const std::string& line) { return line.substr(0, line.find(' ')); }
}

std::vector<fs::path> vk::shaders::GameFiles(const fs::path& game)
{
    std::vector<fs::path> files;
    std::error_code ec;
    for (const auto& entry : fs::directory_iterator(game, ec))
        if (entry.path().extension() == ".ff") files.push_back(entry.path());
    std::sort(files.begin(), files.end());
    for (const char* executable : { "default.xex", "default_mp.xex" }) files.push_back(game / executable);
    return files;
}

std::string vk::shaders::Fingerprint(void* physical, uint32_t scale, const fs::path& game)
{
    std::string text = std::string("build ") + kBuildId + "\n";
#ifdef MW2_HAVE_VULKAN
    VkPhysicalDeviceProperties p{};
    vkGetPhysicalDeviceProperties(static_cast<VkPhysicalDevice>(physical), &p);
    char device[96];
    std::snprintf(device, sizeof device, "%04x:%04x", p.vendorID, p.deviceID);
    char driver[16];
    std::snprintf(driver, sizeof driver, "%08x", p.driverVersion);
    text += "device-id " + vk::pipeline::DeviceId(physical) + "\n";
    text += std::string("device ") + device + " " + p.deviceName + "\n";
    text += std::string("driver ") + driver + "\n";
    text += "cache-format " + Hex(p.pipelineCacheUUID, VK_UUID_SIZE) + "\n";
#else
    (void)physical;
#endif
    text += "scale " + std::to_string(scale) + "\n";
    // A file another of the same name and size replaced has another time.
    for (const fs::path& file : GameFiles(game))
    {
        std::error_code ec;
        const uintmax_t size = fs::file_size(file, ec);
        if (ec) continue;
        const long long written = static_cast<long long>(std::chrono::duration_cast<std::chrono::microseconds>(
            fs::last_write_time(file, ec).time_since_epoch()).count());
        text += "file " + Utf8(file.filename()) + " " + std::to_string(size) + " " + std::to_string(written) + "\n";
    }
    return text;
}

vk::shaders::Marker vk::shaders::ReadMarker()
{
    Marker marker;
    std::ifstream in(kMarkerFile, std::ios::binary);
    if (!in) return marker;
    std::string line;
    while (std::getline(in, line))
    {
        if (line.rfind("shaders ", 0) == 0)
        {
            marker.shaders = uint32_t(std::strtoul(line.c_str() + 8, nullptr, 10));
            marker.found = true;   // the last line: a marker without it is not whole
            break;
        }
        if (line.rfind("device-id ", 0) == 0) marker.device = line.substr(10);
        marker.fingerprint += line + "\n";
    }
    return marker;
}

bool vk::shaders::Compiled(const std::string& fingerprint, std::string* why)
{
    auto say = [&](const char* text) {
        if (why) *why = text;
        return false;
    };
    std::error_code ec;
    const Marker marker = ReadMarker();
    if (!marker.found) return say("The game's shaders have not been compiled for this computer yet.");
    // A device that compiles shaders only with a draw's state had none compiled.
    if (marker.shaders && (fs::file_size(kCacheFile, ec) == 0 || ec)) return say("The compiled shaders are missing.");
    if (marker.fingerprint == fingerprint) return true;
    // The first fact that differs.
    const std::vector<std::string> was = Lines(marker.fingerprint), is = Lines(fingerprint);
    for (size_t i = 0; i < std::max(was.size(), is.size()); i++)
    {
        const std::string a = i < was.size() ? was[i] : "", b = i < is.size() ? is[i] : "";
        if (a == b) continue;
        const std::string key = Key(a.empty() ? b : a);
        if (key == "build") return say("This version of the game draws with other shaders than the last one.");
        if (key == "device-id" || key == "device") return say("The game now draws on another graphics card.");
        if (key == "driver" || key == "cache-format") return say("The graphics driver has changed.");
        if (key == "scale") return say("The resolution has changed, and the shaders are made for it.");
        return say("The game's files have changed.");
    }
    return say("The compiled shaders are for something else.");
}

bool vk::shaders::WriteMarker(const std::string& fingerprint, uint32_t shaders, std::string& error)
{
    const std::string partial = std::string(kMarkerFile) + ".partial";
    {
        std::ofstream out(partial, std::ios::binary | std::ios::trunc);
        out << fingerprint << "shaders " << shaders << "\n";
        out.close();
        if (!out) { error = "the list of compiled shaders could not be written"; return false; }
    }
    std::error_code ec;
    fs::rename(partial, kMarkerFile, ec);
    if (ec) { error = "the list of compiled shaders could not be put in place: " + ec.message(); return false; }
    return true;
}

void vk::shaders::RemoveMarker()
{
    std::error_code ec;
    fs::remove(kMarkerFile, ec);
}

bool vk::shaders::SameTarget(const std::string& a, const std::string& b)
{
    auto target = [](const std::string& text) {
        std::string kept;
        for (const std::string& line : Lines(text))
            if (Key(line) != "file") kept += line + "\n";
        return kept;
    };
    return target(a) == target(b);
}
