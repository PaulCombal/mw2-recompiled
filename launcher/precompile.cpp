#include "precompile.h"
#include "settings.h"
#include "zone/fastfile.h"
#include "zone/shaders.h"
#include "zone/vertex_binding.h"
#include "../runtime/gpu/vulkan/pipeline.h"
#include "../runtime/gpu/vulkan/shader_build.h"
#include "../runtime/gpu/vulkan/shader_marker.h"
#include "../runtime/install/xex.h"

#include <algorithm>
#include <chrono>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <map>
#include <mutex>
#include <thread>
#include <vector>

namespace fs = std::filesystem;

namespace
{
    constexpr const char* kGameFolder = "game";

    double Since(std::chrono::steady_clock::time_point from)
    {
        return std::chrono::duration<double>(std::chrono::steady_clock::now() - from).count();
    }

    std::string Name(const fs::path& path)
    {
        const std::u8string name = path.filename().u8string();
        return std::string(name.begin(), name.end());
    }

    std::vector<uint8_t> Image(const fs::path& path)
    {
        std::ifstream file(path, std::ios::binary);
        if (!file) throw zone::Error("cannot open " + Name(path));
        const std::vector<uint8_t> xex((std::istreambuf_iterator<char>(file)), std::istreambuf_iterator<char>());
        std::vector<uint8_t> image;
        std::string error;
        if (!install::xex::Image(xex, image, error)) throw zone::Error(Name(path) + ": " + error);
        return image;
    }

    // MW2_SCALE as the game takes it: from its environment first, which a
    // game that starts the launcher passes on, then from .env.
    uint32_t AskedScale()
    {
        if (const char* set = std::getenv("MW2_SCALE"))
            return uint32_t(std::clamp(std::strtol(set, nullptr, 10), 1L, long(settings::kMaxScale)));
        return uint32_t(settings::Scale());
    }

    // MW2_CUSTOM_MAPS, the same way.
    bool AskedCustomMaps()
    {
        if (const char* set = std::getenv("MW2_CUSTOM_MAPS")) return *set && std::strcmp(set, "0") != 0;
        return settings::CustomMaps();
    }
}

#ifndef MW2_HAVE_VULKAN
precompile::Result precompile::Run(const std::string&, bool, Progress&)
{
    Result result;
    result.error = "this launcher was built without Vulkan";
    return result;
}
#else

#include <vulkan/vulkan.h>

namespace
{
    // The device to compile for, and the marker a pass on it would leave
    // now: from an instance of its own, without bringing a device up.
    struct Target
    {
        std::string device, fingerprint;
    };
    bool FindTarget(const std::string& asked, Target& target, std::string& error)
    {
        VkApplicationInfo app{ VK_STRUCTURE_TYPE_APPLICATION_INFO };
        app.pApplicationName = "mw2recomp";
        app.apiVersion = VK_API_VERSION_1_2;
        VkInstanceCreateInfo info{ VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO };
        info.pApplicationInfo = &app;
        VkInstance instance = VK_NULL_HANDLE;
        if (vkCreateInstance(&info, nullptr, &instance) != VK_SUCCESS)
        {
            error = "Vulkan is not available on this computer";
            return false;
        }
        // The device the game named; else the one the last pass was for, which
        // a game drawing elsewhere than the window's own choice named then.
        std::string id = asked.empty() ? vk::shaders::ReadMarker().device : asked;
        void* physical = id.empty() ? nullptr : vk::pipeline::ChooseDevice(instance, id, nullptr);
        if (!physical && !asked.empty()) error = "the graphics card the game draws on is not there any more";
        if (!physical && asked.empty()) physical = vk::pipeline::ChooseDevice(instance, "", nullptr);
        if (physical)
        {
            target.device = vk::pipeline::DeviceId(physical);
            target.fingerprint = vk::shaders::Fingerprint(
                physical, vk::shaders::Scale(static_cast<VkPhysicalDevice>(physical), AskedScale()), kGameFolder,
                AskedCustomMaps());
        }
        else if (error.empty()) error = "no graphics card here can draw the game";
        vkDestroyInstance(instance, nullptr);
        return physical != nullptr;
    }
}

precompile::Result precompile::Run(const std::string& asked, bool always, Progress& progress)
{
    Result result;
    Target target;
    if (!FindTarget(asked, target, result.error)) return result;
    result.deviceId = target.device;
    if (vk::shaders::Compiled(target.fingerprint, &result.why) && !always)
    {
        result.ok = result.current = true;
        result.shaders = vk::shaders::ReadMarker().shaders;
        return result;
    }
    if (always) result.why = "Asked for.";
    progress.why = result.why;
    progress.checked = true;

    // A pass that does not finish leaves no marker. The cache of the last one
    // stays for this one to start from, when it was for the same code on the
    // same device and driver at the same scale: only the shaders of files
    // that changed are compiled then. Anything else in it is of no use, and
    // would only grow the file.
    const vk::shaders::Marker before = vk::shaders::ReadMarker();
    vk::shaders::RemoveMarker();
    std::error_code ec;
    if (!before.found || !vk::shaders::SameTarget(before.fingerprint, target.fingerprint))
        fs::remove(vk::shaders::kCacheFile, ec);

    const unsigned cores = std::max(1u, std::thread::hardware_concurrency());
    auto cancelled = [&] {
        if (!progress.cancel) return false;
        result.cancelled = true;
        result.error = "The shaders were not compiled: stopped.";
        return true;
    };

    // ---- every shader of the game, once ------------------------------------
    // A level's zone is up to 160 MB once inflated, so only a few are read at
    // a time.
    const auto readFrom = std::chrono::steady_clock::now();
    std::vector<fs::path> files;
    for (const fs::path& file : vk::shaders::GameFiles(kGameFolder, AskedCustomMaps()))
        if (file.extension() == ".ff") files.push_back(file);
    progress.files = uint32_t(files.size()) + 2;
    std::map<std::pair<bool, uint64_t>, zone::Shader> all;
    std::mutex lock;
    std::string failure;
    // The two executables: the shaders they carry, and the tables vertex
    // shaders are bound with, which must agree.
    zone::BindTables tables;
    try
    {
        bool first = true;
        for (const bool multiplayer : { false, true })
        {
            const std::vector<uint8_t> image =
                Image(fs::path(kGameFolder) / (multiplayer ? "default_mp.xex" : "default.xex"));
            const zone::BindTables read = zone::ReadBindTables(image, multiplayer);
            for (zone::Shader& s : zone::ExecutableShaders(image, multiplayer, read))
                all.try_emplace({ s.pixel, s.hash }, std::move(s));
            if (!first && std::memcmp(&tables, &read, sizeof read))
                throw zone::Error("the two executables bind vertex shaders with different tables");
            tables = read;
            first = false;
            progress.filesRead++;
        }
    }
    catch (const std::exception& e)
    {
        result.error = e.what();
        return result;
    }
    {
        std::atomic<size_t> next{ 0 };
        std::vector<std::thread> readers;
        for (unsigned t = 0; t < std::min(4u, cores); t++)
            readers.emplace_back([&] {
                for (size_t i; !progress.cancel && (i = next++) < files.size();)
                {
                    try
                    {
                        std::vector<zone::Shader> found = zone::Shaders(zone::ReadFastfile(files[i]), tables);
                        std::lock_guard guard(lock);
                        for (zone::Shader& s : found) all.try_emplace({ s.pixel, s.hash }, std::move(s));
                    }
                    catch (const std::exception& e)
                    {
                        // A file of the game the pass cannot read is one whose
                        // shaders it cannot compile: the pass fails, naming it.
                        std::lock_guard guard(lock);
                        if (failure.empty()) failure = Name(files[i]) + ": " + e.what();
                        progress.cancel = true;
                    }
                    progress.filesRead++;
                }
            });
        for (std::thread& t : readers) t.join();
    }
    result.readSeconds = Since(readFrom);
    if (!failure.empty())
    {
        result.error = "A file of the game could not be read, so its shaders cannot be compiled: " + failure;
        return result;
    }
    if (cancelled()) return result;
    result.shaders = uint32_t(all.size());
    progress.shaders = result.shaders;

    // ---- the device the game draws on, with its pipeline cache -------------
    fs::create_directories(fs::path(vk::shaders::kCacheFile).parent_path(), ec);
    vk::pipeline::SetCachePath(vk::shaders::kCacheFile);
    vk::pipeline::SetDevice(target.device);
    if (!vk::pipeline::Initialise())
    {
        result.error = "The graphics card could not be started to compile the shaders on.";
        return result;
    }
    result.device = vk::pipeline::DeviceName();
    const VkPhysicalDevice physical = static_cast<VkPhysicalDevice>(vk::pipeline::PhysicalDevice());
    if (!vk::pipeline::PipelineLibraries())
    {
        // Without pipeline libraries a shader is compiled only as part of a
        // whole pipeline, which needs the state of a draw to make: the game
        // compiles them as it meets them, and there is nothing to do here.
        vk::pipeline::Shutdown();
        result.noLibraries = true;
        result.shaders = 0;
    }
    else
    {
        const VkFormat depth = vk::shaders::DepthFormat(physical);
        const uint32_t scale = vk::shaders::Scale(physical, AskedScale());

        // ---- each built as the game builds it, on every core ---------------
        const auto compileFrom = std::chrono::steady_clock::now();
        std::vector<const zone::Shader*> work;
        for (const auto& [key, s] : all) work.push_back(&s);
        std::atomic<size_t> next{ 0 };
        std::atomic<uint32_t> untranslated{ 0 }, refused{ 0 }, fromCache{ 0 };
        std::vector<std::thread> compilers;
        for (unsigned t = 0; t < cores; t++)
            compilers.emplace_back([&] {
                for (size_t i; !progress.cancel && (i = next++) < work.size();)
                {
                    const zone::Shader& s = *work[i];
                    vk::shaders::Built built = vk::shaders::Build(s.pixel ? shader::Type::Pixel : shader::Type::Vertex,
                                                                  s.code.data(), s.code.size(), scale, depth);
                    if (!built.translation.ok)
                    {
                        untranslated++;
                        std::lock_guard guard(lock);
                        result.reasons[built.translation.error]++;
                    }
                    else if (!built.module || !built.library) refused++;
                    else if (built.fromCache == 1) fromCache++;
                    vk::shaders::Destroy(built);
                    progress.compiled++;
                }
            });
        for (std::thread& t : compilers) t.join();
        result.compileSeconds = Since(compileFrom);
        result.untranslated = untranslated;
        result.refused = refused;
        result.fromCache = fromCache;

        vk::pipeline::Shutdown();   // writes the cache, if it got this far: what is compiled is kept
        if (cancelled()) return result;
        result.cacheBytes = fs::file_size(vk::shaders::kCacheFile, ec);
        if (ec)
        {
            result.error = "The compiled shaders could not be written to " + std::string(vk::shaders::kCacheFile) + ".";
            return result;
        }
    }
    if (!vk::shaders::WriteMarker(target.fingerprint, result.shaders, result.error)) return result;
    result.ok = true;
    return result;
}
#endif
