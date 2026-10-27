// The friends screen (friends.h).
#include "friends.h"
#include "log.h"
#include "online/service.h"

#include <algorithm>
#include <atomic>
#include <mutex>
#include <set>
#include <string>
#include <vector>

namespace
{
    constexpr uint32_t kUsers = 4;
    constexpr uint16_t kUp = 0x0001, kDown = 0x0002, kBack = 0x0020, kGuide = 0x0400, kA = 0x1000, kB = 0x2000;

    std::mutex g_lock;
    std::atomic<bool> g_open{ false };
    online::Service* g_service = nullptr;   // with friends to pick from; else the message
    std::vector<std::string> g_message;
    online::FriendList g_list;
    std::set<uint64_t> g_invited;           // since the screen opened
    int g_focus = 0;
    std::atomic<uint64_t> g_version{ 0 };

    // The controllers, as last read.
    uint16_t g_held[kUsers]{};
    bool g_swallow[kUsers]{};

    void ShutLocked()
    {
        g_open = false;
        // Whatever is held now chose something here, not in the title.
        for (uint32_t user = 0; user < kUsers; user++) g_swallow[user] = g_held[user] != 0;
        g_version++;
        screen::Release(screen::Which::Friends);
    }

    uint64_t ChosenLocked()
    {
        return g_focus < int(g_list.playing.size()) ? g_list.playing[g_focus].id : 0;
    }
    // A new list keeps the friend chosen chosen, wherever they moved to.
    void RefocusLocked(uint64_t chosen)
    {
        const auto at = std::find_if(g_list.playing.begin(), g_list.playing.end(),
                                     [&](const online::Friend& f) { return f.id == chosen; });
        g_focus = at != g_list.playing.end() ? int(at - g_list.playing.begin())
                                             : std::clamp(g_focus, 0, std::max(0, int(g_list.playing.size()) - 1));
        g_version++;
    }

    void InviteLocked()
    {
        if (!g_service || g_focus >= int(g_list.playing.size())) return;
        const online::Friend& chosen = g_list.playing[g_focus];
        if (chosen.withYou || !chosen.elsewhere.empty() || g_invited.contains(chosen.id)) return;
        if (g_service->InviteFriend(chosen.id)) g_invited.insert(chosen.id);
        g_version++;
    }
}

void friends::Ask()
{
    online::Service* service = online::Get();
    std::lock_guard lock(g_lock);
    if (g_open || !screen::Claim(screen::Which::Friends)) return;
    g_list = {};
    g_invited.clear();
    g_focus = 0;
    g_message.clear();
    g_service = service && service->Friends(g_list) ? service : nullptr;
    if (!g_service)
    {
        if (service)
        {
            service->InviteFriends();
            g_message = { "Everyone playing on your network", "was invited.", "",
                          "Choosing which friends to invite", "needs STEAM in the launcher's NETWORK." };
        }
        else
            g_message = { "Inviting friends needs STEAM", "in the launcher's NETWORK." };
    }
    g_open = true;
    g_version++;
    LOGI("friends: the screen opens%s", g_service ? "" : ", with a message: this service has no friends to pick");
}

bool friends::Input(uint32_t user, uint16_t buttons)
{
    if (user >= kUsers) return false;
    std::lock_guard lock(g_lock);
    const uint16_t pressed = buttons & ~g_held[user];
    g_held[user] = buttons;
    if (!g_open)
    {
        if (g_swallow[user]) g_swallow[user] = buttons != 0;
        return g_swallow[user];
    }
    const int count = int(g_list.playing.size());
    if (g_service && count)
    {
        if (pressed & kDown) { g_focus = (g_focus + 1) % count; g_version++; }
        if (pressed & kUp)   { g_focus = (g_focus + count - 1) % count; g_version++; }
    }
    if (pressed & (kB | kBack | kGuide)) ShutLocked();
    else if (pressed & kA)
    {
        if (g_service) InviteLocked();
        else ShutLocked();
    }
    return true;
}

bool friends::Picture(uint32_t windowHeight, screen::Image& image)
{
    if (!screen::Drawn() || !g_open.load(std::memory_order_relaxed)) return false;
    std::lock_guard lock(g_lock);
    if (!g_open) return false;
    // Asked once a frame, which keeps the service's list fresh while it shows.
    if (g_service)
    {
        const uint64_t had = g_list.version, chosen = ChosenLocked();
        g_service->Friends(g_list);
        if (g_list.version != had) RefocusLocked(chosen);
    }

    // A 450 by 400 sheet, a little under two thirds of the window's height.
    const uint32_t height = std::max<uint32_t>(260, windowHeight * 62 / 100) & ~1u;
    const uint64_t version = g_version.load();
    if (image.version == version && image.height == height) return true;
    using screen::Font;
    screen::Sheet sheet(image, height, 450, 400, version);
    sheet.Frame("INVITE FRIENDS");

    if (!g_service)
    {
        for (size_t i = 0; i < g_message.size(); i++)
            sheet.Text(Font::Small, 29, 80 + float(i) * 26, screen::kWhite, g_message[i]);
        sheet.Buttons(374, { { "B", "Back" } });
        screen::Dump("friends", image);
        return true;
    }

    sheet.Text(Font::Small, 29, 64, screen::kDim, "Friends playing Modern Warfare 2: Recompiled");
    const int count = int(g_list.playing.size());
    if (!g_list.version)
        sheet.Text(Font::Entry, 28, 104, screen::kDim, "Looking for your friends...");
    else if (!count)
        sheet.Text(Font::Entry, 28, 104, screen::kDim, "None of your friends is playing.");

    // Five friends are seen at once; the list follows the one chosen.
    constexpr int kSeen = 5;
    constexpr float kRow = 50;
    const int first = std::clamp(g_focus - kSeen + 1, 0, std::max(0, count - kSeen));
    for (int i = first; i < std::min(count, first + kSeen); i++)
    {
        const online::Friend& one = g_list.playing[i];
        const float top = 96 + float(i - first) * kRow;
        const bool chosen = i == g_focus;
        if (chosen)
        {
            sheet.Fill(2, top, 448, top + kRow - 2, screen::kBar);
            sheet.Fill(2, top, 8, top + kRow - 2, screen::kAmber);
        }
        if (one.avatarSide) sheet.Picture(24, top + 4, 40, one.avatar, one.avatarSide);
        else sheet.Fill(24, top + 4, 64, top + 44, screen::kEdge);
        sheet.Text(Font::Entry, 76, top - 1, chosen ? screen::kWhite : screen::kDim, one.name);
        const std::string status = one.withYou                  ? "In your game"
                                   : !one.elsewhere.empty()     ? one.elsewhere + ": cannot join"
                                   : g_invited.contains(one.id) ? "Invited"
                                                                : "";
        sheet.Text(Font::Small, 77, top + 26, one.withYou || g_invited.contains(one.id) ? screen::kAmber : screen::kDim,
                   status);
    }
    if (first > 0) sheet.Text(Font::Small, 410, 74, screen::kDim, "...");
    if (first + kSeen < count) sheet.Text(Font::Small, 410, 340, screen::kDim, "...");

    if (g_list.version)
        sheet.Text(Font::Small, 29, 350, screen::kDim,
                   "Not playing: " + std::to_string(g_list.online) + " online, " + std::to_string(g_list.offline) +
                   " offline");
    sheet.Buttons(374, { { "A", "Invite" }, { "B", "Back" } });
    screen::Dump("friends", image);
    return true;
}
