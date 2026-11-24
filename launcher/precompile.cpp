// The compile pass's C side. The pass itself is Rust (launcher/src/precompile.rs);
// what it needs of the launcher's C++ -- the game's shaders as zone/ reads them,
// the settings, the log, the threads it compiles on -- it calls here, and
// precompile::Run calls it.
//
// Text crosses as bytes and a length, paths as UTF-8, which a path is made from
// as std::filesystem::path(std::u8string) makes it: on Windows the whole name,
// where the code page would lose what it cannot spell. Nothing thrown leaves a
// function here (each is noexcept): a game file's error comes back as its text,
// and a standard exception the C++ pass would have let out of precompile::Run --
// a thread that cannot be started, memory -- is kept and the call says it failed;
// the Rust then returns at once, and Run throws it, as the window and the
// terminal mode expect (main.cpp).
#include "precompile.h"
#include "settings.h"
#include "zone/fastfile.h"
#include "zone/shaders.h"
#include "zone/vertex_binding.h"
#include "../runtime/install/xex.h"
#include "../runtime/log.h"

#include <cstdio>
#include <cstring>
#include <exception>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <thread>
#include <utility>
#include <vector>

namespace fs = std::filesystem;

namespace
{
    // What a function below hands the Rust: a shader of a file, an error's text.
    using ShaderSink = void (*)(void* context, bool pixel, uint64_t hash, const uint32_t* code, size_t count);
    using TextSink = void (*)(void* context, const char* text, size_t length);

    fs::path Path(const char* text, size_t length)
    {
        return fs::path(std::u8string(reinterpret_cast<const char8_t*>(text), length));
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

    void Hand(const std::vector<zone::Shader>& shaders, ShaderSink sink, void* context)
    {
        for (const zone::Shader& s : shaders) sink(context, s.pixel, s.hash, s.code.data(), s.code.size());
    }

    void Say(const std::string& text, TextSink sink, void* context) { sink(context, text.data(), text.size()); }

    // What a call on the pass's thread threw, for Run to throw again.
    thread_local std::exception_ptr thrown;

    template <typename Work> bool Keeping(Work&& work) noexcept
    {
        try
        {
            work();
            return true;
        }
        catch (const std::exception&)
        {
            thrown = std::current_exception();
            return false;
        }
    }

    enum class Count : uint32_t { Files, FilesRead, Shaders, Compiled };
    std::atomic<uint32_t>& Field(precompile::Progress* progress, Count which)
    {
        switch (which)
        {
        case Count::Files: return progress->files;
        case Count::FilesRead: return progress->filesRead;
        case Count::Shaders: return progress->shaders;
        default: return progress->compiled;
        }
    }

    struct Text
    {
        const char* data;
        size_t length;
    };
    struct Summary
    {
        bool ok, cancelled, current, noLibraries;
        uint32_t shaders, untranslated, refused, fromCache;
        double readSeconds, compileSeconds;
        uint64_t cacheBytes;
        Text error, why, device, deviceId;
    };
}

// The launcher's shadow scale goes as far as the shaders crate's
// (shaders/src/shader_build.rs, MAX_SHADOW_SCALE), which precompile.rs holds to 4.
static_assert(settings::kMaxShadowScale == 4);

// What the Rust calls.
extern "C"
{
    // ---- the settings, as main has loaded them (settings::Load) ------------
    int mw2l_scale() noexcept { return settings::Scale(); }
    int mw2l_shadow_scale() noexcept { return settings::ShadowScale(); }
    bool mw2l_custom_maps() noexcept { return settings::CustomMaps(); }
    int mw2l_max_scale() noexcept { return settings::kMaxScale; }
    int mw2l_max_shadow_scale() noexcept { return settings::kMaxShadowScale; }

    // ---- the game's shaders ------------------------------------------------
    // The size of zone::BindTables, which the Rust holds as bytes: two are the
    // same when their bytes are, as memcmp compares them.
    size_t mw2l_bind_tables_size() noexcept { return sizeof(zone::BindTables); }

    // The shaders an executable carries (zone::ExecutableShaders) and, in
    // `tables`, the tables it binds vertex shaders with.
    bool mw2l_executable_shaders(const char* path, size_t length, bool multiplayer, void* tables, ShaderSink sink,
                                 void* context, TextSink error, void* errorContext) noexcept
    {
        try
        {
            const std::vector<uint8_t> image = Image(Path(path, length));
            const zone::BindTables read = zone::ReadBindTables(image, multiplayer);
            Hand(zone::ExecutableShaders(image, multiplayer, read), sink, context);
            std::memcpy(tables, &read, sizeof read);
            return true;
        }
        catch (const std::exception& e)
        {
            Say(e.what(), error, errorContext);
            return false;
        }
    }

    // The shaders of a fastfile (zone::Shaders), their vertex shaders bound with
    // `tables`.
    bool mw2l_fastfile_shaders(const char* path, size_t length, const void* tables, ShaderSink sink, void* context,
                               TextSink error, void* errorContext) noexcept
    {
        try
        {
            zone::BindTables bound;
            std::memcpy(&bound, tables, sizeof bound);
            Hand(zone::Shaders(zone::ReadFastfile(Path(path, length)), bound), sink, context);
            return true;
        }
        catch (const std::exception& e)
        {
            Say(e.what(), error, errorContext);
            return false;
        }
    }

    // ---- what the pass runs on ---------------------------------------------
    unsigned mw2l_cores() noexcept { return std::thread::hardware_concurrency(); }

    // `work(context)` on `count` threads of the C++ library's, as many as the
    // pass starts, until each returns. False when the first could not be
    // started (a later one ends the program, as a joinable std::thread
    // destroyed does).
    bool mw2l_threads(unsigned count, void (*work)(void*), void* context) noexcept
    {
        return Keeping([&] {
            std::vector<std::thread> threads;
            for (unsigned t = 0; t < count; t++) threads.emplace_back([=] { work(context); });
            for (std::thread& t : threads) t.join();
        });
    }

    // A line of the shaders crate's log, as LOGI and the others write one.
    void mw2l_log(const char* line) noexcept
    {
#ifndef MW2_NO_LOGGING
        std::fprintf(stderr, "%s%s\n", mw2log::Stamp(), line);
#else
        (void)line;
#endif
    }

    // ---- progress and result -----------------------------------------------
    bool mw2l_progress_cancelled(precompile::Progress* progress) noexcept { return progress->cancel; }
    void mw2l_progress_cancel(precompile::Progress* progress) noexcept { progress->cancel = true; }
    void mw2l_progress_set(precompile::Progress* progress, Count which, uint32_t value) noexcept
    {
        Field(progress, which) = value;
    }
    void mw2l_progress_add(precompile::Progress* progress, Count which) noexcept { Field(progress, which)++; }
    // What no longer fits, then that the marker has been looked at.
    bool mw2l_progress_checked(precompile::Progress* progress, const char* why, size_t length) noexcept
    {
        return Keeping([&] {
            progress->why.assign(why, length);
            progress->checked = true;
        });
    }

    bool mw2l_result(precompile::Result* result, const Summary* summary) noexcept
    {
        return Keeping([&] {
            const auto text = [](const Text& t) { return std::string(t.data, t.length); };
            result->ok = summary->ok;
            result->cancelled = summary->cancelled;
            result->current = summary->current;
            result->noLibraries = summary->noLibraries;
            result->shaders = summary->shaders;
            result->untranslated = summary->untranslated;
            result->refused = summary->refused;
            result->fromCache = summary->fromCache;
            result->readSeconds = summary->readSeconds;
            result->compileSeconds = summary->compileSeconds;
            result->cacheBytes = summary->cacheBytes;
            result->error = text(summary->error);
            result->why = text(summary->why);
            result->device = text(summary->device);
            result->deviceId = text(summary->deviceId);
        });
    }
    bool mw2l_result_reason(precompile::Result* result, const char* text, size_t length, uint32_t count) noexcept
    {
        return Keeping([&] { result->reasons[std::string(text, length)] = count; });
    }

    // launcher/src/precompile.rs.
    void mw2_precompile_run(const char* device, size_t length, bool always, precompile::Progress* progress,
                            precompile::Result* result);
}

precompile::Result precompile::Run(const std::string& device, bool always, Progress& progress)
{
    Result result;
    thrown = nullptr;
    mw2_precompile_run(device.data(), device.size(), always, &progress, &result);
    if (thrown) std::rethrow_exception(std::exchange(thrown, nullptr));
    return result;
}
