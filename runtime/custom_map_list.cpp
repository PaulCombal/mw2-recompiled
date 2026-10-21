// Custom maps in the multiplayer's menus.
//
// Two lists name the maps. The arena table: UI_LoadArenas parses
// mp/basemaps.arena, a raw file in patch_mp.ff, into up to 64 info strings, and
// UI_UpdateArenas takes each one's map, longname, description and mapimage
// into the table the lobby looks a map's name and picture up in. And the menus
// the host chooses a map from, settings_map (the game's 16) and
// settings_bonus_map (the downloadable ones), also in patch_mp.ff: a button
// each, written out by hand, whose action sets ui_mapname.
//
// A map converted from the PC game comes with its own .arena, in the same
// format, which the PC game reads from its folder (the 360 title keeps only
// that path's message, ".arena files : %s"). With MW2_CUSTOM_MAPS, for every
// <map>.arena in the game folder whose <map>.ff is there too:
//
//   - the arena is parsed after the title's own, with the title's parser, so
//     the lobby knows the map;
//   - the bonus menu gets a button for each map in it, below its own: a copy of
//     its last map button with the map's name in its action, its label and the
//     three local variables its focus sets for the side panel (ui_info_name,
//     ui_info_desc, ui_info_icon). The copy leaves out the downloadable maps'
//     colour expression, which greys a map the console has not bought.
//
// The longname and description are shown as the arena has them: the menus'
// "@" lookup answers them with themselves.
//
// The side panel's picture is a material, preview_<map>, which a stock map
// has in ui_mp.ff and a downloadable one in dlc1_ui_mp.ff or dlc2_ui_mp.ff,
// zones Com_LoadUiFastFile loads in one DB_LoadXAssets call. A converted map
// comes with its own, made of its loading screen: <map>_preview.ff, which
// that call loads too, after the title's, with the same flags (a level's load
// frees them). A map without one shows black.
//
// UI_UpdateArenas runs UI_LoadArenas when the menus first need the table (the
// private match's lobby), not at start, and again when it is marked stale; the
// menu is patched the first time, and again if it is ever loaded anew.
#include <ppc_recomp_shared.h>
#include "title.h"
#include "env.h"
#include "log.h"
#include "kernel/kernel.h"
#include "install/arena.h"

#include <atomic>
#include <cstring>
#include <filesystem>
#include <map>
#include <string>
#include <vector>

#ifdef T_UI_LoadArenas

namespace
{
    constexpr uint32_t kRoom = 64;              // the info strings the table holds
    constexpr uint32_t kBufferSize = 0x18000;   // UI_LoadArenas' own, ARENA_BUFFER_SIZE
    constexpr uint32_t kMenuAsset = 24;

    // menuDef_t and itemDef_s, as the 360 lays them out (launcher/zone/defs/iw4_assets.h).
    constexpr uint32_t kMenuItemCount = 184, kMenuItems = 296;
    constexpr uint32_t kItemSize = 460;
    // rect is the item's position as written; rectClient (+24) the one the
    // menu places and draws it by, made from it when the menu loaded.
    constexpr uint32_t kItemRectY = 8, kItemRectClientY = 28, kItemAction = 328, kItemOnFocus = 336, kItemEnableDvar = 356;
    constexpr uint32_t kItemFloatExpressionCount = 396, kItemFloatExpressions = 400, kItemTextExp = 412;
    // A button 20 units below the one before; the menu's frame ends at 384.
    constexpr float kRowHeight = 20, kListEnd = 368;

    constexpr uint8_t kEventUnconditional = 0, kEventSetLocalVarString = 6;
    constexpr uint32_t kOperand = 1, kValString = 2;

    // XZoneInfo { name, allocFlags, freeFlags }, as DB_LoadXAssets takes a list of them.
    constexpr uint32_t kZoneInfoSize = 12;
    constexpr uint32_t kMostZones = 32;   // what the loader holds: one more stops the game

    using arena::Map;

    std::string Utf8(const std::filesystem::path& path)
    {
        const std::u8string text = path.u8string();
        return std::string(text.begin(), text.end());
    }

    uint32_t Alloc(uint32_t size)
    {
        const uint32_t at = kernel::AllocateGuest(size, 16);
        if (at) std::memset(guest::Base() + at, 0, size);
        return at;
    }

    uint32_t GuestString(const std::string& text)
    {
        const uint32_t at = Alloc(uint32_t(text.size()) + 1);
        if (at) std::memcpy(guest::Base() + at, text.c_str(), text.size() + 1);
        return at;
    }

    bool Equals(uint8_t* base, uint32_t string, const std::string& text)
    {
        return string && std::strcmp(reinterpret_cast<const char*>(base + string), text.c_str()) == 0;
    }

    // A copy of a Statement_s whose string operands equal to `from` are `to`.
    uint32_t CopyStatement(uint8_t* base, uint32_t statement, const std::string& from, uint32_t to)
    {
        if (!statement) return 0;
        const uint32_t copy = Alloc(24);
        std::memcpy(base + copy, base + statement, 24);
        const uint32_t count = PPC_LOAD_U32(statement), entries = PPC_LOAD_U32(statement + 4);
        const uint32_t made = Alloc(count * 12);
        std::memcpy(base + made, base + entries, count * 12);
        for (uint32_t i = 0; i < count; i++)
        {
            const uint32_t entry = made + i * 12;
            if (PPC_LOAD_U32(entry) == kOperand && PPC_LOAD_U32(entry + 4) == kValString &&
                Equals(base, PPC_LOAD_U32(entry + 8), from))
                PPC_STORE_U32(entry + 8, to);
        }
        PPC_STORE_U32(copy + 4, made);
        return copy;
    }

    // The string a statement holds, the first one: the map's own in a button's.
    std::string StatementString(uint8_t* base, uint32_t statement, size_t nth)
    {
        if (!statement) return {};
        const uint32_t count = PPC_LOAD_U32(statement), entries = PPC_LOAD_U32(statement + 4);
        for (uint32_t i = 0; i < count; i++)
        {
            const uint32_t entry = entries + i * 12;
            if (PPC_LOAD_U32(entry) == kOperand && PPC_LOAD_U32(entry + 4) == kValString && nth-- == 0)
                return reinterpret_cast<const char*>(base + PPC_LOAD_U32(entry + 8));
        }
        return {};
    }

    // MenuEventHandlerSet { count, MenuEventHandler** }; MenuEventHandler { data, type }.
    uint32_t NewHandlerSet(uint8_t* base, const std::vector<uint32_t>& handlers)
    {
        const uint32_t set = Alloc(8), list = Alloc(uint32_t(handlers.size()) * 4);
        PPC_STORE_U32(set, uint32_t(handlers.size()));
        PPC_STORE_U32(set + 4, list);
        for (size_t i = 0; i < handlers.size(); i++) PPC_STORE_U32(list + uint32_t(i) * 4, handlers[i]);
        return set;
    }

    uint32_t NewScript(uint8_t* base, const std::string& script)
    {
        const uint32_t handler = Alloc(8);
        PPC_STORE_U32(handler, GuestString(script));
        base[handler + 4] = kEventUnconditional;
        return handler;
    }

    // The maps whose picture the menus' zones have, by map, and the picture:
    // <name>_preview.ff loaded with them, named after its .arena as the
    // launcher installs it, holds preview_<name>. Made anew each time the
    // menus load; written and read on the thread that loads them.
    std::map<std::string, std::string> g_previews;

    // The template's focus handlers, with the side panel's three variables
    // set to the map's.
    uint32_t CopyFocus(uint8_t* base, uint32_t set, const Map& map, const std::string (&was)[3])
    {
        static const char* const kVariables[3] = { "ui_info_name", "ui_info_desc", "ui_info_icon" };
        const auto picture = g_previews.find(map.map);
        const std::string is[3] = { map.longname, map.description, picture != g_previews.end() ? picture->second : "black" };
        std::vector<uint32_t> handlers;
        const uint32_t count = PPC_LOAD_U32(set), list = PPC_LOAD_U32(set + 4);
        for (uint32_t i = 0; i < count; i++)
        {
            uint32_t handler = PPC_LOAD_U32(list + i * 4);
            const uint32_t data = PPC_LOAD_U32(handler);
            if (base[handler + 4] == kEventSetLocalVarString)
                for (int v = 0; v < 3; v++)
                    if (Equals(base, PPC_LOAD_U32(data), kVariables[v]))
                    {
                        // SetLocalVarData { name, expression }
                        const uint32_t setVar = Alloc(8);
                        PPC_STORE_U32(setVar, PPC_LOAD_U32(data));
                        PPC_STORE_U32(setVar + 4, CopyStatement(base, PPC_LOAD_U32(data + 4), was[v], GuestString(is[v])));
                        handler = Alloc(8);
                        PPC_STORE_U32(handler, setVar);
                        base[handler + 4] = kEventSetLocalVarString;
                    }
            handlers.push_back(handler);
        }
        return NewHandlerSet(base, handlers);
    }

    float LoadFloat(uint8_t* base, uint32_t at)
    {
        const uint32_t word = PPC_LOAD_U32(at);
        float value;
        std::memcpy(&value, &word, 4);
        return value;
    }

    void StoreFloat(uint8_t* base, uint32_t at, float value)
    {
        uint32_t word;
        std::memcpy(&word, &value, 4);
        PPC_STORE_U32(at, word);
    }

    uint32_t g_patchedItems = 0;   // the item array this hook last gave the bonus menu

    // The longnames and descriptions the custom maps show, which the menus
    // look up as translations. Written once, before the first lookup that
    // could want them, and only read after.
    std::vector<std::string> g_texts;
    std::atomic<bool> g_textsReady{ false };

    // The bonus menu's own buttons and then one for each custom map.
    void AddButtons(PPCContext& ctx, uint8_t* base, const std::vector<Map>& maps)
    {
        const PPCContext saved = ctx;
        const uint32_t name = ctx.r1.u32 - 512;
        std::strcpy(reinterpret_cast<char*>(base + name), "settings_bonus_map");
        ctx.r1.u64 = ctx.r1.u32 - 1024;
        ctx.r3.u64 = kMenuAsset;
        ctx.r4.u64 = name;
        GUEST_FUNC(T_DB_FindXAssetHeader)(ctx, base);
        const uint32_t menu = ctx.r3.u32;
        ctx = saved;
        if (!menu) { LOGW("maps: no settings_bonus_map menu"); return; }
        const uint32_t items = PPC_LOAD_U32(menu + kMenuItems), count = PPC_LOAD_U32(menu + kMenuItemCount);
        if (items == g_patchedItems) return;

        // The template: the last button whose action sets ui_mapname.
        uint32_t last = 0, lastIndex = 0;
        for (uint32_t i = 0; i < count; i++)
        {
            const uint32_t item = PPC_LOAD_U32(items + i * 4);
            if (PPC_LOAD_U32(item + kItemEnableDvar) && PPC_LOAD_U32(item + kItemAction) && PPC_LOAD_U32(item + kItemOnFocus) &&
                !StatementString(base, PPC_LOAD_U32(item + kItemTextExp), 1).empty())
            { last = item; lastIndex = i; }
        }
        if (!last) { LOGW("maps: settings_bonus_map has no map button to copy"); return; }
        const std::string label = StatementString(base, PPC_LOAD_U32(last + kItemTextExp), 1);
        // What the template's focus sets, to find the strings to replace.
        std::string was[3];
        {
            const uint32_t set = PPC_LOAD_U32(last + kItemOnFocus);
            const uint32_t n = PPC_LOAD_U32(set), list = PPC_LOAD_U32(set + 4);
            static const char* const kVariables[3] = { "ui_info_name", "ui_info_desc", "ui_info_icon" };
            for (uint32_t i = 0; i < n; i++)
            {
                const uint32_t handler = PPC_LOAD_U32(list + i * 4), data = PPC_LOAD_U32(handler);
                if (base[handler + 4] != kEventSetLocalVarString) continue;
                for (int v = 0; v < 3; v++)
                    if (Equals(base, PPC_LOAD_U32(data), kVariables[v])) was[v] = StatementString(base, PPC_LOAD_U32(data + 4), 0);
            }
        }
        const uint32_t click = PPC_LOAD_U32(PPC_LOAD_U32(PPC_LOAD_U32(last + kItemAction) + 4));   // "play" "mouse_click"

        std::vector<uint32_t> added;
        float y = LoadFloat(base, last + kItemRectY) + kRowHeight;
        for (const Map& map : maps)
        {
            if (y > kListEnd) { LOGW("maps: no room in the menu for %s", map.map.c_str()); continue; }
            const uint32_t item = Alloc(kItemSize);
            std::memcpy(base + item, base + last, kItemSize);
            StoreFloat(base, item + kItemRectY, y);
            StoreFloat(base, item + kItemRectClientY, LoadFloat(base, last + kItemRectClientY) + (y - LoadFloat(base, last + kItemRectY)));
            y += kRowHeight;
            PPC_STORE_U32(item + kItemEnableDvar, GuestString("\"" + map.map + "\" "));
            PPC_STORE_U32(item + kItemFloatExpressionCount, 0);
            PPC_STORE_U32(item + kItemFloatExpressions, 0);
            PPC_STORE_U32(item + kItemTextExp, CopyStatement(base, PPC_LOAD_U32(last + kItemTextExp), label, GuestString(map.longname)));
            PPC_STORE_U32(item + kItemAction, NewHandlerSet(base, { click, NewScript(base,
                "\"setdvar\" \"ui_mapname\" \"" + map.map + "\" ; \"close\" \"settings_map\" ; \"close\" \"self\" ; ") }));
            PPC_STORE_U32(item + kItemOnFocus, CopyFocus(base, PPC_LOAD_U32(last + kItemOnFocus), map, was));
            added.push_back(item);
        }
        if (added.empty()) return;

        // The new buttons go after the template, where the menu's own order
        // of focus has them follow it.
        const uint32_t made = Alloc((count + uint32_t(added.size())) * 4);
        uint32_t at = 0;
        for (uint32_t i = 0; i < count; i++)
        {
            PPC_STORE_U32(made + at++ * 4, PPC_LOAD_U32(items + i * 4));
            if (i == lastIndex)
                for (uint32_t item : added) PPC_STORE_U32(made + at++ * 4, item);
        }
        PPC_STORE_U32(menu + kMenuItems, made);
        PPC_STORE_U32(menu + kMenuItemCount, count + uint32_t(added.size()));
        g_patchedItems = made;
        LOGI("maps: %zu custom map%s in the bonus maps menu", added.size(), added.size() == 1 ? "" : "s");
    }
}

GUEST_HOOK(T_UI_LoadArenas)
{
    GUEST_ORIG(T_UI_LoadArenas)(ctx, base);
    static const bool customMaps = env::Flag("MW2_CUSTOM_MAPS");
    if (!customMaps) return;

    const PPCContext saved = ctx;
    // A frame of its own below the caller's, for the buffer's handle.
    ctx.r1.u64 = saved.r1.u32 - 128;
    const uint32_t handle = ctx.r1.u32 + 80;
    ctx.r3.u64 = handle;
    ctx.r4.u64 = kBufferSize;
    GUEST_FUNC(T_LargeLocal_Begin)(ctx, base);
    ctx.r3.u64 = handle;
    GUEST_FUNC(T_LargeLocal_GetBuf)(ctx, base);
    const uint32_t buffer = ctx.r3.u32;

    std::vector<Map> maps;
    for (const std::filesystem::path& file : arena::Installed(kernel::GameRoot()))
    {
        const uint32_t count = PPC_LOAD_U32(T_DATA_UI_ArenaInfos + 256);
        const std::string name = Utf8(file.filename());
        if (count >= kRoom)
        {
            LOGW("maps: %s left out, the map list is full", name.c_str());
            break;
        }
        const std::string text = arena::Read(file);
        if (text.size() >= kBufferSize)
        {
            LOGW("maps: %s left out, it is larger than the title's %u bytes", name.c_str(), kBufferSize);
            continue;
        }
        std::memcpy(base + buffer, text.data(), text.size());
        base[buffer + text.size()] = 0;
        ctx.r3.u64 = buffer;
        ctx.r4.u64 = kRoom - count;
        ctx.r5.u64 = T_DATA_UI_ArenaInfos + count * 4;
        GUEST_FUNC(T_UI_ParseInfos)(ctx, base);
        const uint32_t added = ctx.r3.u32;
        PPC_STORE_U32(T_DATA_UI_ArenaInfos + 256, count + added);
        for (const Map& map : arena::Maps(text)) maps.push_back(map);
        LOGI("maps: %s adds %u map%s to the list", name.c_str(), added, added == 1 ? "" : "s");
    }

    ctx.r3.u64 = handle;
    GUEST_FUNC(T_LargeLocal_End)(ctx, base);
    ctx = saved;
    if (!maps.empty() && !g_textsReady)
    {
        for (const Map& map : maps)
            for (const std::string* text : { &map.longname, &map.description })
                if (!text->empty()) g_texts.push_back(*text);
        g_textsReady = true;
    }
    if (!maps.empty()) AddButtons(ctx, base, maps);
}

// The menus' zones: Com_LoadUiFastFile's one DB_LoadXAssets call gets each
// installed map's picture after them, while the loader has room. (The loader
// copies the list's names, 64 bytes each, before it returns.)
namespace
{
    thread_local bool t_loadingMenus = false;
    constexpr uint32_t kZoneNameSize = 64;
}

GUEST_HOOK(T_Com_LoadUiFastFile)
{
    t_loadingMenus = true;
    GUEST_ORIG(T_Com_LoadUiFastFile)(ctx, base);
    t_loadingMenus = false;
}

GUEST_HOOK(T_DB_LoadXAssets)
{
    static const bool customMaps = env::Flag("MW2_CUSTOM_MAPS");
    const uint32_t zones = ctx.r3.u32, count = ctx.r4.u32;
    uint32_t menus = 0;   // ui_mp's entry
    for (uint32_t i = 0; i < count && t_loadingMenus && !menus; i++)
        if (Equals(base, PPC_LOAD_U32(zones + i * kZoneInfoSize), "ui_mp")) menus = zones + i * kZoneInfoSize;
    if (!customMaps || !menus)
    {
        GUEST_ORIG(T_DB_LoadXAssets)(ctx, base);
        return;
    }

    std::vector<std::string> previews;
    g_previews.clear();
    const uint32_t loaded = PPC_LOAD_U32(T_DATA_DB_ZoneCount);
    for (const std::filesystem::path& file : arena::Installed(kernel::GameRoot()))
    {
        const std::string name = Utf8(file.stem()), zone = name + "_preview";
        std::filesystem::path zoneFile = file;
        zoneFile.replace_filename(file.stem().native() + std::filesystem::path("_preview.ff").native());
        std::error_code ec;
        if (zone.size() >= kZoneNameSize || !std::filesystem::is_regular_file(zoneFile, ec)) continue;
        if (loaded + count + previews.size() >= kMostZones)
        {
            LOGW("maps: no room among the game's zones for %s's picture", name.c_str());
            continue;
        }
        previews.push_back(zone);
        for (const Map& map : arena::Maps(arena::Read(file))) g_previews.emplace(map.map, "preview_" + name);
    }
    if (previews.empty())
    {
        GUEST_ORIG(T_DB_LoadXAssets)(ctx, base);
        return;
    }

    // The title's list on the stack, and the pictures' after it, each loaded
    // and freed as the menus are; their names below.
    const PPCContext saved = ctx;
    const uint32_t total = count + uint32_t(previews.size());
    const uint32_t names = (saved.r1.u32 - uint32_t(previews.size()) * kZoneNameSize - 64) & ~15u;
    const uint32_t list = (names - total * kZoneInfoSize) & ~15u;
    const uint32_t flags = PPC_LOAD_U32(menus + 4);
    std::memmove(base + list, base + zones, count * kZoneInfoSize);
    for (size_t i = 0; i < previews.size(); i++)
    {
        const uint32_t at = list + (count + uint32_t(i)) * kZoneInfoSize, name = names + uint32_t(i) * kZoneNameSize;
        std::memset(base + name, 0, kZoneNameSize);
        std::memcpy(base + name, previews[i].data(), previews[i].size());
        PPC_STORE_U32(at, name);
        PPC_STORE_U32(at + 4, flags);
        PPC_STORE_U32(at + 8, 0);
    }
    ctx.r1.u64 = list - 128;
    ctx.r3.u64 = list;
    ctx.r4.u64 = total;
    GUEST_ORIG(T_DB_LoadXAssets)(ctx, base);
    const uint64_t result = ctx.r3.u64;
    ctx = saved;
    ctx.r3.u64 = result;
    LOGI("maps: %zu custom map picture%s with the menus' %u zones, %u of the loader's %u loaded before them", previews.size(),
         previews.size() == 1 ? "" : "s", count, loaded, kMostZones);
}

// A custom map's longname and description are no key of the localize assets:
// the "@" lookup the menus show them through finds nothing, and the menus mark
// that "UNLOCALIZED(...)". They translate to themselves. A key the game has
// keeps its own translation.
GUEST_HOOK(T_SEH_StringEd_GetString)
{
    const uint32_t key = ctx.r3.u32;
    GUEST_ORIG(T_SEH_StringEd_GetString)(ctx, base);
    if (ctx.r3.u32 || !key || !g_textsReady) return;
    const char* text = reinterpret_cast<const char*>(base + key);
    for (const std::string& own : g_texts)
        if (own == text) { ctx.r3.u64 = key; return; }
}

#endif
