// tools/launcher_diff.py's C++ half: the launcher's C++ as it was before it was Rust,
// compiled from git, with calls for launcher_diff.rs to hold the Rust to. Compiled once a
// module, with OLD the old source file and one of MODULE_SETTINGS, MODULE_PROFILE,
// MODULE_REPORT and MODULE_SOUND; each includes that file whole, so that what it kept in
// an anonymous namespace is in reach here.
#include OLD

#include <cstdint>
#include <cstring>
#include <fstream>
#include <iterator>
#include <sstream>
#include <string>
#include <vector>

#ifdef MODULE_SETTINGS
// setup.cpp's, which the Rust side's text comes back through; and what the Rust's profile
// calls, which the rig does not take.
extern "C" void mw2l_setup_text(std::string* to, const char* text, size_t length) { to->assign(text, length); }
extern "C" void mw2l_profile_player() {}
extern "C" void mw2l_profile_other() {}

extern "C" void old_settings_load() { settings::Load(); }

// The numbers and switches, by the order launcher_diff.rs asks for them.
extern "C" int old_settings_get(int which)
{
    switch (which)
    {
    case 0: return settings::Fullscreen();
    case 1: return settings::Scale();
    case 2: return int(settings::AspectRatio());
    case 3: return settings::ResolutionRow();
    case 4: return settings::HasFpsLimit();
    case 5: return settings::FpsLimit();
    case 6: return settings::Fov();
    case 7: return settings::Msaa();
    case 8: return settings::ShadowScale();
    case 9: return settings::Anisotropy();
    case 10: return settings::Stretch();
    case 11: return settings::CustomMaps();
    case 12: return settings::Sounds();
    case 13: return int(settings::CurrentNetwork());
    case 14: return int(settings::CurrentResolution().width);
    case 15: return int(settings::CurrentResolution().height);
    case 16: return settings::CurrentResolution().scale;
    }
    return -1;
}

// The texts: false for MW2_PC_GAME without a line.
extern "C" bool old_settings_text(int which, std::string* out)
{
    switch (which)
    {
    case 0:
        if (const auto folder = settings::PcGame()) { *out = *folder; return true; }
        return false;
    case 1: *out = settings::Server(); return true;
    case 2: *out = settings::ServerPassword(); return true;
    }
    return false;
}

extern "C" void old_settings_set(int which, int value, int other)
{
    switch (which)
    {
    case 0: settings::SetFullscreen(value); break;
    case 1: settings::SetResolution(settings::Ratio(value), other); break;
    case 2: settings::NextAspectRatio(); break;
    case 3: settings::SetFpsLimit(value); break;
    case 4: settings::SetFov(value); break;
    case 5: settings::SetMsaa(value); break;
    case 6: settings::SetShadowScale(value); break;
    case 7: settings::SetAnisotropy(value); break;
    case 8: settings::SetStretch(value); break;
    case 9: settings::SetCustomMaps(value); break;
    case 10: settings::SetNetwork(settings::Network(value)); break;
    }
}

extern "C" void old_settings_set_text(int which, const char* text, size_t length)
{
    const std::string value(text, length);
    switch (which)
    {
    case 0: settings::SetPcGame(value); break;
    case 1: settings::SetServer(value); break;
    case 2: settings::SetServerPassword(value); break;
    }
}
#endif

#ifdef MODULE_PROFILE
namespace
{
    std::vector<profile::Player> g_players;
}

using PlayerSink = void (*)(void* context, const char* name, size_t nameLength, uint64_t id, const char* file,
                            size_t fileLength, bool offline, bool first, int level, int prestige, const char* played,
                            size_t playedLength, int also);

// The players, listed again and kept for the calls below; each handed to `sink`, with how
// many other files it has, and each of those after it as a player of its own with no name.
extern "C" size_t old_profile_list(PlayerSink sink, void* context)
{
    g_players = profile::Players();
    for (const profile::Player& p : g_players)
    {
        const std::string file = p.file.string();
        sink(context, p.name.data(), p.name.size(), p.id, file.data(), file.size(), p.offline, p.first, p.level, p.prestige,
             p.played.data(), p.played.size(), int(p.also.size()));
        for (const profile::Player::Other& o : p.also)
        {
            const std::string other = o.file.string();
            sink(context, "", 0, 0, other.data(), other.size(), o.offline, false, o.level, o.prestige, o.played.data(),
                 o.played.size(), -1);
        }
    }
    return g_players.size();
}

extern "C" bool old_profile_rename(size_t index, const char* name, size_t length, std::string* error)
{
    return profile::Rename(g_players.at(index), std::string(name, length), *error);
}
extern "C" bool old_profile_play_as(size_t index, std::string* error) { return profile::PlayAs(g_players.at(index), *error); }
extern "C" bool old_profile_set_rank(size_t index, int level, int prestige, std::string* error)
{
    return profile::SetRank(g_players.at(index), level, prestige, *error);
}
extern "C" bool old_profile_unlock(size_t index, std::string* error)
{
    return profile::UnlockEverything(g_players.at(index), *error);
}
extern "C" bool old_profile_campaign(int* missions, int* stars)
{
    const profile::Campaign campaign = profile::ReadCampaign();
    *missions = campaign.missions;
    *stars = campaign.stars;
    return campaign.found;
}
extern "C" bool old_profile_unlock_missions(std::string* error) { return profile::UnlockMissions(*error); }
extern "C" bool old_profile_all_stars(std::string* error) { return profile::AllStars(*error); }
extern "C" int old_profile_max(int which) { return which ? profile::MaxPrestige() : profile::MaxLevel(); }
#endif

#ifdef MODULE_REPORT
// What report.cpp's Make took of the log, as it took it.
namespace update
{
    const char* Current() { return ""; }
    const char* Kind() { return ""; }
}
namespace setup
{
    bool UsesUpdate() { return true; }
}

extern "C" void old_scrub(const char* path, std::string* out)
{
    std::ifstream raw(path, std::ios::binary);
    *out = Scrub(raw);
}

extern "C" void old_first_line(const char* text, size_t length, const char* marker, std::string* out)
{
    *out = FirstLine(std::string(text, length), marker);
}

// Make's own loop, as it is there.
extern "C" void old_performance(const char* text, size_t length, std::string* out)
{
    const std::string log(text, length);
    std::string performance;
    {
        std::istringstream lines(log);
        for (std::string line; std::getline(lines, line);)
            if (const size_t at = line.find("] report: "); at != std::string::npos && line.find("report: graphics") == std::string::npos)
                performance += "- " + line.substr(at + 10) + "\n";
    }
    *out = performance;
}

// And its test of which game wrote the log.
extern "C" bool old_multiplayer(const char* text, size_t length)
{
    const std::string log(text, length);
    return log.find("runtime (multiplayer") < log.find('\n');
}

extern "C" void old_encoded(const char* text, size_t length, std::string* out) { *out = Encoded(std::string(text, length)); }

extern "C" void old_file_address(const char* path, size_t length, std::string* out)
{
    *out = FileAddress(fs::path(std::string(path, length)));
}
#endif

#ifdef MODULE_SOUND
// (settings::Sounds is the old settings', linked in.)
namespace setup
{
    std::filesystem::path GameFolder() { return "game"; }
}
extern "C" void mw2l_decode_xma(const uint8_t*, size_t, void (*)(void*, const float*, size_t), void*) {}

// Read()'s way to a sound, as it is there, up to its decoding: the fastfile's zone, and the
// sound found in it -- where its buffer is in the zone, its size and its rate. -2 when the
// file is not a fastfile it takes or does not inflate, -1 when the sound is not found.
extern "C" int old_find(const char* path, const char* name, size_t* at, size_t* size, int* rate)
{
    std::ifstream file(path, std::ios::binary);
    if (!file) return -2;
    const std::vector<uint8_t> packed((std::istreambuf_iterator<char>(file)), std::istreambuf_iterator<char>());
    if (packed.size() <= kZlib || std::memcmp(packed.data(), kMagic, sizeof(kMagic)) || Big32(packed.data() + kStreamedImages)) return -2;
    std::vector<uint8_t> zone;
    Inflater inflater{ packed.data() + kZlib, packed.size() - kZlib };
    if (!inflater.Run(zone)) return -2;
    const Found found = Find(zone, name);
    if (!found.data) return -1;
    *at = size_t(found.data - zone.data());
    *size = found.size;
    *rate = found.rate;
    return 0;
}
#endif

#ifdef MODULE_SETTINGS
// The std::strings launcher_diff.rs hands the C++ to fill.
extern "C" std::string* launcher_diff_string_new() { return new std::string; }
extern "C" void launcher_diff_string_free(std::string* text) { delete text; }
extern "C" const char* launcher_diff_string_data(std::string* text, size_t* length)
{
    *length = text->size();
    return text->data();
}
#endif
