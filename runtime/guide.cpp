// The guide (guide.h).
#include "guide.h"
#include "crash.h"
#include "friends.h"
#include "log.h"
#include "presence.h"
#include "signin.h"
#include "online/service.h"

#include <algorithm>
#include <atomic>
#include <cstring>
#include <ctime>
#include <mutex>
#include <string>

namespace
{
    constexpr uint32_t kUsers = 4;
    constexpr uint16_t kUp = 0x0001, kDown = 0x0002, kBack = 0x0020, kA = 0x1000, kB = 0x2000;

    enum class Item { Resume, Invite, Quit };
    constexpr Item kItems[] = { Item::Resume, Item::Invite, Item::Quit };
    constexpr int kCount = int(std::size(kItems));

    std::mutex g_lock;
    std::atomic<bool> g_open{ false };
    int g_focus = 0;
    bool g_confirming = false;   // Quit chosen once; A again quits
    std::atomic<uint64_t> g_version{ 0 };

    // What the picture shows that changes while it is open.
    std::string g_clock, g_presence, g_network;
    online::FriendList g_friends;

    uint16_t g_held[kUsers]{};
    bool g_swallow[kUsers]{};

    void OpenLocked()
    {
        if (!screen::Claim(screen::Which::Guide)) return;
        g_open = true;
        g_focus = 0;
        g_confirming = false;
        g_version++;
        LOGI("guide: opens");
    }

    // `next` is the screen that takes over, if any.
    void ShutLocked(screen::Which next = screen::Which::None)
    {
        g_open = false;
        for (uint32_t user = 0; user < kUsers; user++) g_swallow[user] = g_held[user] != 0;
        g_version++;
        if (next == screen::Which::None) screen::Release(screen::Which::Guide);
        else screen::Pass(screen::Which::Guide, next);
    }

    void ChooseLocked()
    {
        switch (kItems[g_focus])
        {
        case Item::Resume:
            ShutLocked();
            break;
        case Item::Invite:
            ShutLocked(screen::Which::Friends);
            friends::Ask();
            break;
        case Item::Quit:
            if (!g_confirming) { g_confirming = true; g_version++; break; }
            LOGI("guide: the player quits");
            crash::RequestExit("the player quit from the guide");
            break;
        }
    }

    std::string Clock()
    {
        const std::time_t now = std::time(nullptr);
        std::tm local{};
#ifdef _WIN32
        localtime_s(&local, &now);
#else
        localtime_r(&now, &local);
#endif
        char text[8];
        std::strftime(text, sizeof text, "%H:%M", &local);
        return text;
    }

    std::string Plural(int count, const char* one) { return std::to_string(count) + " " + one + (count == 1 ? "" : "s"); }

    // "Steam: 2 friends playing, 5 online", or the network alone.
    std::string Network(online::Service* service)
    {
        const char* name = online::Name();
        if (!name) return "No network: system link on this PC";
        if (std::strcmp(name, "lan") == 0) return "LAN";
        if (std::strcmp(name, "server") == 0) return "Server";
        if (!service || !service->Friends(g_friends) || !g_friends.version) return "Steam";
        const int playing = int(g_friends.playing.size());
        return "Steam: " + Plural(playing, "friend") + " playing, " + std::to_string(g_friends.online) + " online";
    }

    // Keeps a picture's text current, and says whether it changed.
    void Refresh(std::string& held, std::string now)
    {
        if (held == now) return;
        held = std::move(now);
        g_version++;
    }
}

bool guide::Input(uint32_t user, uint16_t buttons)
{
    if (user >= kUsers) return false;
    std::lock_guard lock(g_lock);
    const uint16_t pressed = buttons & ~g_held[user];
    g_held[user] = buttons;
    if (!g_open)
    {
        // Not while what shut a screen is still held: Back and Start pressed
        // one after the other would shut it with Back and open it again.
        if (user == 0 && (pressed & signin::kGuide) && !g_swallow[user])
        {
            OpenLocked();
            if (g_open) return true;
        }
        if (g_swallow[user]) g_swallow[user] = buttons != 0;
        return g_swallow[user];
    }
    // The first controller's; the others wait.
    if (user != 0) return true;
    if (pressed & (kDown | kUp))
    {
        g_focus = (g_focus + ((pressed & kDown) ? 1 : kCount - 1)) % kCount;
        g_confirming = false;
        g_version++;
    }
    if (pressed & signin::kGuide) ShutLocked();
    else if (pressed & (kB | kBack))
    {
        if (g_confirming) { g_confirming = false; g_version++; }
        else ShutLocked();
    }
    else if (pressed & kA) ChooseLocked();
    return true;
}

bool guide::Picture(uint32_t windowHeight, screen::Image& image)
{
    if (!screen::Drawn() || !g_open.load(std::memory_order_relaxed)) return false;
    std::lock_guard lock(g_lock);
    if (!g_open) return false;
    Refresh(g_clock, Clock());
    Refresh(g_presence, presence::Text(0));
    Refresh(g_network, Network(online::Get()));

    // A 450 by 300 sheet, under half the window's height.
    const uint32_t height = std::max<uint32_t>(200, windowHeight * 46 / 100) & ~1u;
    const uint64_t version = g_version.load();
    if (image.version == version && image.height == height) return true;
    using screen::Font;
    screen::Sheet sheet(image, height, 450, 300, version);
    online::Service* service = online::Get();
    const std::string name = service && !service->LocalName().empty() ? service->LocalName() : signin::First().name;
    sheet.Frame(name);
    sheet.Text(Font::Entry, 422 - sheet.Width(Font::Entry, g_clock), 24, screen::kDim, g_clock);

    sheet.Text(Font::Small, 29, 66, screen::kWhite,
               g_presence.empty() ? "Modern Warfare 2: Recompiled" : g_presence);
    sheet.Text(Font::Small, 29, 90, screen::kDim, g_network);
    sheet.Fill(28, 124, 422, 125, screen::kEdge);

    for (int i = 0; i < kCount; i++)
    {
        const float top = 140 + float(i) * 36;
        const bool chosen = i == g_focus;
        if (chosen)
        {
            sheet.Fill(2, top, 448, top + 34, screen::kBar);
            sheet.Fill(2, top, 8, top + 34, screen::kAmber);
        }
        const char* label = kItems[i] == Item::Resume ? "Resume"
                          : kItems[i] == Item::Invite ? "Invite friends"
                          : g_confirming              ? "Quit to desktop? A again to quit"
                                                      : "Quit to desktop";
        sheet.Text(Font::Entry, 28, top + 2, chosen ? screen::kWhite : screen::kDim, label);
    }
    sheet.Buttons(272, { { "A", "Select" }, { "B", "Back" } });
    screen::Dump("guide", image);
    return true;
}
