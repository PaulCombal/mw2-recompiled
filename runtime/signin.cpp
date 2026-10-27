// The sign-in screen and who it signed in (signin.h).
#include "signin.h"
#include "log.h"
#include "kernel/kernel.h"
#include "env.h"
#include "screen.h"

#include <algorithm>
#include <atomic>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <mutex>
#include <random>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#endif


namespace
{
    using namespace signin;

    constexpr uint16_t kUp = 0x0001, kDown = 0x0002, kStart = 0x0010, kA = 0x1000, kB = 0x2000, kX = 0x4000;
    constexpr uint32_t XN_SYS_SIGNINCHANGED = 0x0000000A;

    struct Profile { uint64_t id; std::string name; };

    struct Item
    {
        enum What { Existing, New, SignOut } what;
        Profile profile;
        std::string label;
    };

    std::mutex g_lock;
    Player g_players[kUsers];

    // The screen.
    std::atomic<bool> g_open{ false };
    uint32_t g_asking = 0;
    std::vector<Item> g_items;
    int g_focus = 0;
    std::atomic<uint64_t> g_version{ 0 };

    // The controllers, as last read.
    uint16_t g_held[kUsers]{};
    bool g_swallow[kUsers]{};
    uint32_t g_pressedLast = 0;

    std::filesystem::path File() { return kernel::SaveRoot() / "profiles.txt"; }

    // The file: a line per profile, twelve hexadecimal digits, a space, the
    // name; `first <the first controller's>`; `machine <where it was written>`.
    struct Book
    {
        std::vector<Profile> profiles;
        uint64_t first = 0, machine = 0;
    };
    Book Read()
    {
        Book book;
        std::ifstream in(File());
        for (std::string line; std::getline(in, line); )
        {
            while (!line.empty() && (line.back() == '\r' || line.back() == ' ')) line.pop_back();
            char* end = nullptr;
            if (line.rfind("first ", 0) == 0) { book.first = std::strtoull(line.c_str() + 6, &end, 16); continue; }
            if (line.rfind("machine ", 0) == 0) { book.machine = std::strtoull(line.c_str() + 8, &end, 16); continue; }
            if (line.size() < 14 || line[12] != ' ') continue;
            const uint64_t id = std::strtoull(line.substr(0, 12).c_str(), &end, 16);
            if (!id || *end) continue;
            book.profiles.push_back({ id, line.substr(13, 15) });   // a gamertag's longest
        }
        return book;
    }
    std::vector<Profile> Profiles() { return Read().profiles; }

    void Write(const Book& book)
    {
        std::error_code ec;
        std::filesystem::create_directories(kernel::SaveRoot(), ec);
        std::ofstream out(File(), std::ios::trunc);
        char line[64];
        std::snprintf(line, sizeof line, "machine %016llx\nfirst %012llx\n", (unsigned long long)book.machine,
                      (unsigned long long)book.first);
        out << line;
        for (const Profile& profile : book.profiles)
        {
            std::snprintf(line, sizeof line, "%012llx %s\n", (unsigned long long)profile.id, profile.name.c_str());
            out << line;
        }
        if (!out) LOGE("signin: cannot write %s", File().string().c_str());
    }

    // A number no profile of the book has.
    uint64_t NewId(const Book& book)
    {
        std::random_device random;
        uint64_t id = 0;
        do id = ((uint64_t(random()) << 32) | random()) & 0xFFFFFFFFFFFFull;
        while (id < 0x10000 || std::any_of(book.profiles.begin(), book.profiles.end(), [&](const Profile& p) { return p.id == id; }));
        return id;
    }

    uint64_t Fnv(const std::string& text)
    {
        uint64_t hash = 0xCBF29CE484222325ull;   // FNV-1a
        for (char c : text) hash = (hash ^ uint8_t(c)) * 0x100000001B3ull;
        return hash;
    }

    // This machine among others, as its system names it.
    uint64_t Machine()
    {
        std::string id;
#ifdef _WIN32
        char text[64] = {};
        DWORD size = sizeof text - 1;
        if (RegGetValueA(HKEY_LOCAL_MACHINE, "SOFTWARE\\Microsoft\\Cryptography", "MachineGuid", RRF_RT_REG_SZ,
                         nullptr, text, &size) == ERROR_SUCCESS)
            id = text;
#else
        std::ifstream("/etc/machine-id") >> id;
#endif
        const uint64_t hash = id.empty() ? 0 : Fnv(id);
        return hash ? hash : 1;
    }

    // The profile at the first controller, read or made once a run.
    Player g_first;
    const Player& FirstLocked()
    {
        if (g_first.signedIn) return g_first;
        Book book = Read();
        const uint64_t machine = Machine();
        bool changed = book.machine != machine;
        // Written on another machine: these numbers are that machine's players.
        if (book.machine && book.machine != machine)
        {
            for (Profile& profile : book.profiles)
            {
                const uint64_t was = profile.id;
                profile.id = NewId(book);
                if (book.first == was) book.first = profile.id;
                kernel::MovePlayerData(was, profile.id);
            }
            LOGI("signin: the profiles were another machine's; %zu have new numbers here", book.profiles.size());
        }
        book.machine = machine;
        auto first = std::find_if(book.profiles.begin(), book.profiles.end(), [&](const Profile& p) { return p.id == book.first; });
        if (first == book.profiles.end())
        {
            Profile made;
            made.id = NewId(book);
            const char* name = env::Text("MW2_NAME");
#ifdef _WIN32
            if (!name) name = std::getenv("USERNAME");
#else
            if (!name) name = std::getenv("USER");
#endif
            made.name = name && *name ? std::string(name).substr(0, 15) : "Player 1";
            book.profiles.insert(book.profiles.begin(), made);
            book.first = made.id;
            first = book.profiles.begin();
            changed = true;
            LOGI("signin: a profile for the first controller, %s", made.name.c_str());
        }
        if (changed) Write(book);
        g_first = { true, first->id, first->name };
        return g_first;
    }

    // "Player 2", or the first number after it nobody has.
    Profile NewProfile()
    {
        const std::vector<Profile> profiles = Profiles();
        Profile made;
        for (int number = 2; ; number++)
        {
            made.name = "Player " + std::to_string(number);
            if (std::none_of(profiles.begin(), profiles.end(), [&](const Profile& p) { return p.name == made.name; }))
                break;
        }
        Book book;
        book.profiles = profiles;
        made.id = NewId(book);

        std::error_code ec;
        std::filesystem::create_directories(kernel::SaveRoot(), ec);
        char line[64];
        std::snprintf(line, sizeof line, "%012llx %s\n", (unsigned long long)made.id, made.name.c_str());
        std::ofstream out(File(), std::ios::app);
        out << line;
        if (!out) LOGE("signin: cannot write %s", File().string().c_str());
        return made;
    }

    uint32_t MaskLocked()
    {
        uint32_t mask = 1;
        for (uint32_t user = 1; user < kUsers; user++)
            if (g_players[user].signedIn) mask |= 1u << user;
        return mask;
    }

    void ShutLocked()
    {
        g_open = false;
        g_items.clear();
        // Whatever is held now chose something here, not in the title.
        for (uint32_t user = 0; user < kUsers; user++) g_swallow[user] = g_held[user] != 0;
        g_version++;
        screen::Release(screen::Which::Signin);
    }

    void ChooseLocked(const Item& item)
    {
        Player& player = g_players[g_asking];
        Player chosen;
        switch (item.what)
        {
        case Item::Existing:
            chosen = { true, item.profile.id, item.profile.name };
            break;
        case Item::New:
        {
            const Profile made = NewProfile();
            chosen = { true, made.id, made.name };
            break;
        }
        case Item::SignOut:
            break;
        }
        player = chosen;
        LOGI("signin: controller %u %s%s", g_asking, chosen.signedIn ? "signs in as " : "signs out", chosen.name.c_str());
        ShutLocked();
        kernel::PostNotification(XN_SYS_SIGNINCHANGED, MaskLocked());
    }
    void OpenLocked(uint32_t user)
    {
        // The first controller's player is the online service's.
        if (g_open || user == 0 || user >= kUsers || !screen::Claim(screen::Which::Signin)) return;
        g_asking = user;
        const Player& current = g_players[g_asking];

        g_items.clear();
        for (const Profile& profile : Profiles())
        {
            const bool taken = profile.id == FirstLocked().id ||
                               std::any_of(g_players, g_players + kUsers, [&](const Player& other) {
                                   return other.signedIn && other.id == profile.id; });
            if (!taken) g_items.push_back({ Item::Existing, profile, profile.name });
        }
        g_items.push_back({ Item::New, {}, "New profile" });
        if (current.signedIn) g_items.push_back({ Item::SignOut, {}, "Sign out" });
        g_focus = 0;
        g_open = true;
        g_version++;
        LOGI("signin: the screen opens for controller %u, %zu entries", g_asking, g_items.size());
    }
}

signin::Player signin::At(uint32_t user)
{
    if (user == 0 || user >= kUsers) return {};
    std::lock_guard lock(g_lock);
    return g_players[user];
}

signin::Player signin::First()
{
    std::lock_guard lock(g_lock);
    return FirstLocked();
}

uint32_t signin::Mask()
{
    std::lock_guard lock(g_lock);
    return MaskLocked();
}

void signin::Ask()
{
    std::lock_guard lock(g_lock);
    OpenLocked(g_pressedLast);
}

bool signin::Input(uint32_t user, uint16_t buttons)
{
    if (user >= kUsers) return false;
    std::lock_guard lock(g_lock);
    const uint16_t pressed = buttons & ~g_held[user];
    g_held[user] = buttons;
    if (!g_open)
    {
        if (pressed & (kA | kX | kStart)) g_pressedLast = user;
        // The console's own button opens the screen whatever the title shows;
        // not while what shut a screen is still held, which Back and Start
        // pressed one after the other would otherwise be.
        if ((pressed & kGuide) && !g_swallow[user])
        {
            OpenLocked(user);
            if (g_open) return true;
        }
        if (g_swallow[user]) g_swallow[user] = buttons != 0;
        return g_swallow[user];
    }
    if (user == g_asking)
    {
        const int count = int(g_items.size());
        if (pressed & kDown) { g_focus = (g_focus + 1) % count; g_version++; }
        if (pressed & kUp)   { g_focus = (g_focus + count - 1) % count; g_version++; }
        if (pressed & kA) ChooseLocked(Item(g_items[g_focus]));
        else if (pressed & kB) ShutLocked();
    }
    return true;
}

bool signin::Picture(uint32_t windowHeight, Image& image)
{
    if (!screen::Drawn() || !g_open.load(std::memory_order_relaxed)) return false;
    std::lock_guard lock(g_lock);
    if (!g_open) return false;

    // A 450 by 360 sheet, a little over half the window's height.
    const uint32_t height = std::max<uint32_t>(240, windowHeight * 56 / 100) & ~1u;
    const uint64_t version = g_version.load();
    if (image.version == version && image.height == height) return true;
    using screen::Font;
    screen::Sheet sheet(image, height, 450, 360, version);
    sheet.Frame("SIGN IN");
    const Player& current = g_players[g_asking];
    const std::string who = "Controller " + std::to_string(g_asking + 1) +
        (current.signedIn ? ", signed in as " + current.name : "");
    sheet.Text(Font::Small, 29, 64, screen::kDim, who);

    // Six entries are seen at once; the list follows the one chosen.
    constexpr int kSeen = 6;
    const int count = int(g_items.size());
    const int first = std::clamp(g_focus - kSeen + 1, 0, std::max(0, count - kSeen));
    for (int i = first; i < std::min(count, first + kSeen); i++)
    {
        const float top = 104.0f + float(i - first) * 36.0f;
        const bool chosen = i == g_focus;
        if (chosen)
        {
            sheet.Fill(2, top, 448, top + 34, screen::kBar);
            sheet.Fill(2, top, 8, top + 34, screen::kAmber);
        }
        sheet.Text(Font::Entry, 28, top + 2, chosen ? screen::kWhite : screen::kDim, g_items[i].label);
    }
    if (first > 0) sheet.Text(Font::Small, 410, 84, screen::kDim, "...");
    if (first + kSeen < count) sheet.Text(Font::Small, 410, 318, screen::kDim, "...");
    sheet.Buttons(326, { { "A", "Select" }, { "B", "Back" } });
    screen::Dump("signin", image);
    return true;
}
