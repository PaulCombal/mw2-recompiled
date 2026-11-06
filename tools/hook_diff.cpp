// Runs a hook's C++ and its Rust port on the same guest states and compares what
// they did. Most ported hooks guard what only a hostile peer sends, which no
// scripted run reaches. tools/hook_diff.py builds this and says how to read it.
//
// Both are given a state of awkward values: a context, a guest stack, strings and
// words that are pointers into each other. The title's functions they call are
// stubs that act on a hash of the whole context, as a real function clobbers the
// registers and the stack below r1, so a wrong argument or frame changes everything
// after it. What has to match: the context, every byte of the 4 GB, every call out
// with the context it was made with, and the log.
//
// The 4 GB are read-only and a fault records the page, so a store is seen wherever
// it lands. Hooks run in a random order in one process: a log-once and the Huffman
// ceiling are state that carries from one hook to the next.
#include <ppc_context.h>

#include <algorithm>
#include <csetjmp>
#include <csignal>
#include <cstdint>
#include <execinfo.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <map>
#include <string>
#include <vector>

#include <sys/mman.h>
#include <unistd.h>

namespace
{
    using Function = void (*)(PPCContext&, uint8_t*);

    struct Hook
    {
        const char* name;
        Function side[2];
    };

    struct Datum
    {
        const char* name;
        uint32_t address;
    };

    struct Stub
    {
        const char* name;   // T_x, or __imp__T_x for a hook's original
    };

    void StubCalled(int id, PPCContext& ctx, uint8_t* base);
}

#include "hook_diff_table.h"   // kHooks, kStubs, kData, and a function per stub
#include "kernel/kernel.h"

// src/runtime/exports.rs's, built with --cfg mw2_hook_rig.
extern "C" void mw2_rig_reset_heap();
extern "C" void mw2_rig_set_game_root(const char* root);

namespace
{
    constexpr uint64_t kMapped = (1ull << 32) + 0x10000;
    constexpr uint32_t kPage = 0x1000;
    constexpr size_t kMostPages = 1 << 18;
    constexpr uint32_t kArena = 0x40000000, kArenaSize = 0x60000;
    constexpr uint32_t kStack = kArena + 0x50000, kText = kArena + 0x58000;

    uint8_t* g_base;
    std::vector<uint32_t> g_pages;                 // the pages a case starts from
    std::map<uint32_t, std::vector<uint8_t>> g_master;
    const std::vector<uint8_t> g_zeros(kPage);
    std::vector<uint64_t> g_dirty;                 // past the 4 GB, a store can cross the top

    int g_side;
    jmp_buf g_escape;
    FILE* g_report;
    int g_log;

    struct Random
    {
        uint64_t state;
        uint64_t Next()
        {
            uint64_t z = (state += 0x9E3779B97F4A7C15ull);
            z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ull;
            z = (z ^ (z >> 27)) * 0x94D049BB133111EBull;
            return z ^ (z >> 31);
        }
        uint32_t Below(uint32_t n) { return uint32_t(Next() % n); }
    };

    constexpr uint32_t kEdges[] = {
        0, 1, 2, 4, 5, 6, 8, 12, 17, 18, 19, 20, 127, 128, 129, 130, 209, 210, 211, 254, 255, 256, 1023, 1024, 1025,
        2047, 2048, 2049, 20000, 20001, 0xFFFF, 0x10000, 0x10001, 0x7FFFFFFF, 0x80000000, 0x80000001, 0xFFFFFFFE, 0xFFFFFFFF,
    };

    // What a guest value tends to be.
    uint32_t Word(Random& random)
    {
        switch (random.Below(8))
        {
        case 0: case 1: case 2: return kArena + random.Below(0x28000);
        case 3: return kText + random.Below(0x2000);
        case 4: return kEdges[random.Below(std::size(kEdges))];
        case 5: return random.Below(40);
        case 6: return uint32_t(random.Next()) | 0xFFFFFF00u;   // small negatives
        default: return uint32_t(random.Next());
        }
    }

    constexpr const char* kTexts[] = {
        "", "mp_dust2", "mp_rust", "nextmap", "NEXTMAP", "NextMap", "nextmap2", "nextma", "com_errorResolveCommand",
        "COM_ERRORRESOLVECOMMAND", "com_errorresolvecommandx", "r_texFilterAnisoMin", "activeAction",
        "mp;quit", "a\"b;c\"d;e", "\"a;b\"", "\"", "x\ny", "x\ry", ";", "\n", "\r", "a;b\nc", "\xC9\xE9;\xFF",
        "mods/custom", "unsigned", "map mp_rust;quit\n", "[{a}]", "x[{}]y", "[{name}", "}]", "[{a}][{b}]c", "[[{a}}]",
    };

    std::vector<uint32_t> g_changed;   // the pages the guest does not hold yet
    uint32_t g_entityType;             // what MSG_EntityTypeFields gives this case

    void FillPage(Random& random, uint32_t page)
    {
        g_changed.push_back(page);
        auto& bytes = g_master[page];
        bytes.resize(kPage);
        for (uint32_t at = 0; at < kPage; at += 4)
        {
            const uint32_t word = __builtin_bswap32(Word(random));
            std::memcpy(bytes.data() + at, &word, 4);
        }
        if (page < kText || page >= kArena + kArenaSize)
            return;
        // The text pages hold strings, end to end, of the list and of noise.
        uint32_t at = 0;
        while (at + 64 < kPage)
        {
            if (random.Below(2))
            {
                const char* text = kTexts[random.Below(std::size(kTexts))];
                const size_t size = std::strlen(text) + 1;
                std::memcpy(bytes.data() + at, text, size);
                at += uint32_t(size);
            }
            else
            {
                const uint32_t size = random.Below(40);
                for (uint32_t i = 0; i < size; i++)
                    bytes[at + i] = "abc;\"\n\r %\xE9"[random.Below(10)];
                bytes[at + size] = 0;
                at += size + 1;
            }
        }
    }

    // A new state: all of it at first, then the title's data and a few pages of
    // the rest, which keeps a case cheap and leaves the memory just as mixed.
    void Fill(Random& random, bool first)
    {
        for (const uint32_t page : g_pages)
            if (first || page >= kArena + kArenaSize || (page < kArena && page) || random.Below(12) == 0)
                FillPage(random, page);
    }

    void Protect(uint64_t page, int protection) { mprotect(g_base + page, kPage, protection); }

    void Load()
    {
        for (const uint32_t page : g_changed)
        {
            Protect(page, PROT_READ | PROT_WRITE);
            std::memcpy(g_base + page, g_master[page].data(), kPage);
            Protect(page, PROT_READ);
        }
        g_changed.clear();
    }

    const uint8_t* Pristine(uint64_t page)
    {
        const auto found = g_master.find(uint32_t(page));
        return found == g_master.end() ? g_zeros.data() : found->second.data();
    }

    using Pages = std::map<uint64_t, std::vector<uint8_t>>;

    // What the side wrote, and the pages back as they were.
    Pages Harvest()
    {
        Pages written;
        for (const uint64_t page : g_dirty)
        {
            written[page].assign(g_base + page, g_base + page + kPage);
            if (page < (1ull << 32) && g_master.count(uint32_t(page)))
                std::memcpy(g_base + page, Pristine(page), kPage);
            else
                madvise(g_base + page, kPage, MADV_DONTNEED);   // a page outside the state goes back to zero
            Protect(page, PROT_READ);
        }
        g_dirty.clear();
        return written;
    }

    const char* g_running = "";

    void Fault(int, siginfo_t* info, void*)
    {
        const auto at = reinterpret_cast<uintptr_t>(info->si_addr) - reinterpret_cast<uintptr_t>(g_base);
        if (at >= kMapped)
        {
            char line[120];
            write(fileno(g_report), line, std::snprintf(line, sizeof line, "a fault outside the guest, at %p, on side %d of %s\n", info->si_addr, g_side, g_running));
            void* frames[32];
            backtrace_symbols_fd(frames, backtrace(frames, 32), fileno(g_report));
            signal(SIGSEGV, SIG_DFL);
            return;
        }
        // More than a case can touch: the same page faulting again, not stores.
        if (g_dirty.size() >= kMostPages)
        {
            write(fileno(g_report), "too many pages written in one case\n", 35);
            _exit(3);
        }
        const uint64_t page = at & ~uint64_t(kPage - 1);
        g_dirty.push_back(page);
        Protect(page, PROT_READ | PROT_WRITE);
    }

    uint64_t Hash(const void* data, size_t bytes, uint64_t hash = 0xCBF29CE484222325ull)
    {
        const auto* p = static_cast<const uint8_t*>(data);
        for (size_t at = 0; at + 8 <= bytes; at += 8)
        {
            uint64_t word;
            std::memcpy(&word, p + at, 8);
            hash = (hash ^ word) * 0x9E3779B97F4A7C15ull;
            hash ^= hash >> 29;
        }
        return hash;
    }

    struct Call
    {
        int stub;
        uint64_t context, stack, text;
        bool operator==(const Call&) const = default;
    };
    std::vector<Call> g_calls;

    uint32_t Load32(uint32_t address)
    {
        uint32_t value;
        std::memcpy(&value, g_base + address, 4);
        return __builtin_bswap32(value);
    }

    void Store32(uint32_t address, uint32_t value)
    {
        value = __builtin_bswap32(value);
        std::memcpy(g_base + address, &value, 4);
    }

    bool IsStub(int id, const char* name) { return !std::strcmp(kStubs[id].name, name); }

    // The custom maps' hooks (custom_map_list): the longnames and descriptions
    // tools/hook_diff.py writes into its .arena files, which the string lookup is
    // asked for.
    constexpr const char* kMapTexts[] = { "Dust II", "Crash Remix", "A map of sand and doors", "@MPUI_AFGHAN", "x",
                                          "Underpass at night", "\xC9t\xE9", "MPUI_DESC_MAP_RUST" };

    const Hook* FindHook(const char* name)
    {
        for (const Hook& hook : kHooks)
            if (!std::strcmp(hook.name, name)) return &hook;
        return nullptr;
    }

    uint32_t Named(const char* name);

    // What the bonus menu's item array was the last time a case patched it: a
    // menu loaded with it is one the hook has patched already.
    uint32_t g_lastMade;
    constexpr uint32_t kMenu = kArena + 0x20000, kMenuEnd = kArena + 0x28000, kArenaBuffer = kArena + 0x30000;

    struct Bump
    {
        uint32_t at;
        uint32_t Take(uint32_t size) { const uint32_t was = at; at = (at + size + 3) & ~3u; return was; }
    };

    uint32_t Text(Bump& bump, const std::string& text)
    {
        const uint32_t at = bump.Take(uint32_t(text.size()) + 1);
        std::memcpy(g_base + at, text.c_str(), text.size() + 1);
        return at;
    }

    // A Statement_s of string operands and others, `strings` of them strings.
    uint32_t Statement(Random& random, Bump& bump, const std::vector<std::string>& strings)
    {
        constexpr const char* kOthers[] = { "", "@MENU_X", "mp_rust", "black", "preview_mp_rust" };
        const uint32_t statement = bump.Take(24), count = uint32_t(strings.size()) + random.Below(3);
        const uint32_t entries = bump.Take(count * 12);
        size_t next = 0;
        for (uint32_t i = 0; i < count; i++)
        {
            const uint32_t entry = entries + i * 12;
            const bool string = next < strings.size() && (random.Below(3) || count - i == strings.size() - next);
            if (string)
            {
                Store32(entry, 1);
                Store32(entry + 4, 2);
                Store32(entry + 8, Text(bump, strings[next++]));
            }
            else
            {
                // An operator, a number, or a string where another field says it is none.
                Store32(entry, random.Below(2));
                Store32(entry + 4, random.Below(2) ? 2 : random.Below(5));
                Store32(entry + 8, random.Below(2) ? Text(bump, kOthers[random.Below(std::size(kOthers))]) : random.Below(9));
            }
        }
        Store32(statement, count);
        Store32(statement + 4, entries);
        for (uint32_t at = 8; at < 24; at += 4) Store32(statement + at, uint32_t(random.Next()));
        return statement;
    }

    uint32_t HandlerSet(Bump& bump, const std::vector<uint32_t>& handlers)
    {
        const uint32_t set = bump.Take(8), list = bump.Take(uint32_t(handlers.size()) * 4);
        Store32(set, uint32_t(handlers.size()));
        Store32(set + 4, list);
        for (size_t i = 0; i < handlers.size(); i++) Store32(list + uint32_t(i) * 4, handlers[i]);
        return set;
    }

    uint32_t Handler(Bump& bump, uint32_t data, uint8_t type)
    {
        const uint32_t handler = bump.Take(8);
        Store32(handler, data);
        g_base[handler + 4] = type;
        return handler;
    }

    // settings_bonus_map as the menu loader leaves it: map buttons, each with an
    // action, a focus that sets the side panel's variables and a label, among items
    // that are not buttons, or not whole ones.
    uint32_t Menu(Random& random)
    {
        constexpr const char* kMaps[] = { "mp_rust", "mp_complex", "mp_abandon", "" };
        constexpr const char* kVariables[] = { "ui_info_name", "ui_info_desc", "ui_info_icon", "ui_info_other", "ui_info_nam" };
        Bump bump{ kMenu + 0x3000 };
        const uint32_t count = random.Below(8) ? 1 + random.Below(20) : 0;
        const uint32_t items = kMenu + 0x200;
        for (uint32_t at = 0; at < 0x200; at += 4) Store32(kMenu + at, uint32_t(random.Next()));
        Store32(kMenu + 184, count);
        Store32(kMenu + 296, g_lastMade && random.Below(4) == 0 ? g_lastMade : items);
        for (uint32_t i = 0; i < count; i++)
        {
            const uint32_t item = kMenu + 0x400 + i * 464;
            Store32(items + i * 4, item);
            for (uint32_t at = 0; at < 460; at += 4) Store32(item + at, uint32_t(random.Next()));
            const std::string map = kMaps[random.Below(std::size(kMaps))];
            const bool button = random.Below(4) != 0;
            // Where the menu places it: rows of 20, either side of the end of the list.
            // 348 puts the next row at 368, the last the list takes.
            float y = random.Below(8) == 0 ? 348.0f : float(int(random.Below(20))) * 20.0f + (random.Below(8) ? 0.0f : 0.5f);
            const float client = y + 40.0f;
            if (random.Below(16) == 0) y = __builtin_nanf("");
            uint32_t bits;
            std::memcpy(&bits, &y, 4);
            Store32(item + 8, bits);
            std::memcpy(&bits, &client, 4);
            Store32(item + 28, bits);
            Store32(item + 356, button || random.Below(2) ? Text(bump, "\"" + map + "\" ") : 0);
            const uint32_t click = Handler(bump, Text(bump, "play \"mouse_click\""), 0);
            const uint32_t script = Handler(bump, Text(bump, "\"setdvar\" \"ui_mapname\" \"" + map + "\""), 0);
            Store32(item + 328, button || random.Below(2) ? HandlerSet(bump, { click, script }) : 0);
            std::vector<uint32_t> focus;
            for (uint32_t h = random.Below(6); h > 0; h--)
            {
                const uint32_t data = bump.Take(8);
                Store32(data, random.Below(8) ? Text(bump, kVariables[random.Below(std::size(kVariables))]) : 0);
                const std::string value = random.Below(3) == 0 ? "@MPUI_" + map : random.Below(2) ? map : "preview_" + map;
                Store32(data + 4, random.Below(8) ? Statement(random, bump, { value }) : 0);
                focus.push_back(Handler(bump, data, uint8_t(random.Below(4) ? 6 : random.Below(8))));
            }
            Store32(item + 336, button || random.Below(2) ? HandlerSet(bump, focus) : 0);
            const uint32_t strings = button ? 2 + random.Below(2) : random.Below(3);
            std::vector<std::string> label;
            for (uint32_t n = 0; n < strings; n++) label.push_back(n == 1 && random.Below(4) ? "@MPUI_" + map : kMaps[random.Below(4)]);
            Store32(item + 412, random.Below(16) ? Statement(random, bump, label) : 0);
            if (bump.at >= kMenuEnd - 0x400) { Store32(kMenu + 184, i + 1); break; }
        }
        return kMenu;
    }

    // Com_LoadUiFastFile's one call: its zone list, with ui_mp in it or not.
    void LoadUiZones(PPCContext& ctx, uint8_t* base, Random& random)
    {
        constexpr const char* kZones[] = { "code_post_gfx_mp", "ui_mp", "common_mp", "dlc1_ui_mp", "ui_mpx", "UI_MP", "" };
        Bump bump{ kMenu };
        const uint32_t count = 1 + random.Below(8), zones = bump.Take(count * 12);
        for (uint32_t i = 0; i < count; i++)
        {
            Store32(zones + i * 12, Text(bump, kZones[random.Below(std::size(kZones))]));
            Store32(zones + i * 12 + 4, uint32_t(random.Next()));
            Store32(zones + i * 12 + 8, uint32_t(random.Next()));
        }
        if (random.Below(4)) Store32(zones + random.Below(count) * 12, Text(bump, "ui_mp"));
        Store32(Named("T_DATA_DB_ZoneCount"), random.Below(8) ? random.Below(34) : uint32_t(random.Next()));
        const uint64_t sp = ctx.r1.u64;
        ctx.r3.u64 = zones;
        ctx.r4.u64 = count;
        ctx.r1.u64 = (sp - 0x100) & ~0xFull;
        FindHook("T_DB_LoadXAssets")->side[g_side](ctx, base);
        ctx.r1.u64 = sp;
    }

    void StubCalled(int id, PPCContext& ctx, uint8_t* base)
    {
        Call call{ id, Hash(&ctx, sizeof ctx), 0, 0 };
        // What the guest is shown: the frame the call was given and its strings.
        const uint32_t sp = ctx.r1.u32;
        if (sp >= 0x100 && sp < 0xFFFF0000)
            call.stack = Hash(base + sp - 0x100, 0x200);
        for (const uint32_t pointer : { ctx.r3.u32, ctx.r4.u32, ctx.r5.u32, ctx.r6.u32 })
            if (pointer >= kArena && pointer < kArena + kArenaSize - 64)
                call.text = Hash(base + pointer, 48, call.text);
        g_calls.push_back(call);
        // With the pacing notes logged (MW2_TRACE_PACING, src/runtime/pacing_trace.rs), the
        // calls are too, so that a note's place before or after a call is compared.
        static const bool pacing = std::getenv("MW2_TRACE_PACING") != nullptr;
        if (pacing) std::fprintf(stderr, "[call] %s\n", kStubs[id].name);

        Random random{ call.context ^ call.stack ^ call.text };
        if (sp >= 0x100 && sp < 0xFFFF0000)   // a callee's frame
            for (uint32_t at = sp - 0x40; at < sp; at += 8)
                Store32(at, uint32_t(random.Next()));

        const auto read = [&] { return random.Below(2) ? uint32_t(int32_t(random.Below(24)) - 2) : Word(random); };
        PPCRegister* volatile_registers[] = { &ctx.r0, &ctx.r3, &ctx.r4, &ctx.r5, &ctx.r6, &ctx.r7,
                                              &ctx.r8, &ctx.r9, &ctx.r10, &ctx.r11, &ctx.r12 };
        const uint32_t from = ctx.r3.u32, to = ctx.r4.u32, size = ctx.r5.u32, cursor = ctx.r6.u32;
        for (PPCRegister* reg : volatile_registers) reg->u64 = read();
        ctx.f1.f64 = double(int32_t(random.Next())) / 4.0;

        if (IsStub(id, "T_Huffman_DecodeSymbol"))
        {
            Store32(to, uint32_t(random.Next()));
            // Now and then a byte a symbol, which fills a buffer to the last byte.
            Store32(cursor, Load32(cursor) + (from & 1 ? 8 : 1 + random.Below(16)));
        }
        else if (IsStub(id, "__imp__T_SV_ParseClientMessage") || IsStub(id, "__imp__T_CL_ParseServerMessage"))
        {
            // The parse decodes its message unless it ends first, and the title
            // may longjmp out of it, which leaves the ceiling as it was.
            if (random.Below(4))
            {
                ctx.r3.u64 = from;
                ctx.r4.u64 = to;
                ctx.r5.u64 = size;
                kReadBitsCompress[g_side](ctx, base);
                if (random.Below(4) == 0)   // a ceiling is good for one decode
                    kReadBitsCompress[g_side](ctx, base);
            }
            if (random.Below(8) == 0)
                longjmp(g_escape, 1);
        }
        else if (IsStub(id, "T_Com_Error") || IsStub(id, "__imp__T_Com_Error"))
        {
            // The levels a port raises leave by the title's longjmp. To see what a
            // port does if it ever came back, HOOK_DIFF_ERROR_RETURNS lets it.
            if (g_side == 0 || !std::getenv("HOOK_DIFF_ERROR_RETURNS"))
                longjmp(g_escape, 1);
        }
        else if (IsStub(id, "T_I_stricmp") || IsStub(id, "T_I_strnicmp"))
        {
            ctx.r3.u64 = random.Below(3) == 0 ? 0 : 1;   // as often the one path as the others
        }
        else if (IsStub(id, "T_MSG_ReadBit") || IsStub(id, "T_MSG_ReadBits") || IsStub(id, "T_MSG_ReadByte") ||
                 IsStub(id, "T_MSG_ReadShort") || IsStub(id, "T_MSG_ReadLong") || IsStub(id, "T_MSG_ReadString"))
        {
            // What the title's readers can give: -1 past the end, and in a signed
            // register nothing wider than was asked for. The C++ runtime is built
            // without -fwrapv, so a value they cannot give would compare undefined
            // behaviour. Each moves the message's read state, which a peek puts back.
            const bool past = random.Below(8) == 0;
            const uint32_t length = 1 + random.Below(40);
            if (IsStub(id, "T_MSG_ReadBit"))
                ctx.r3.u64 = past ? 0xFFFFFFFFu : random.Below(2);
            else if (IsStub(id, "T_MSG_ReadBits"))
                ctx.r3.u64 = to == 11 ? 1021 + random.Below(6) : past ? 0xFFFFFFFFu : random.Below(1u << std::min(to, 31u));
            else if (IsStub(id, "T_MSG_ReadByte"))
                ctx.r3.u64 = random.Below(4) ? random.Below(3) : read();
            else if (IsStub(id, "T_MSG_ReadShort"))
                ctx.r3.u64 = past ? uint32_t(int32_t(random.Below(3)) - 1) : length;
            if (from >= kArena && from < kArena + 0x28000)
            {
                Store32(from, random.Below(8) == 0);
                // A frame's length either side of what is left of the packet.
                Store32(from + 28, IsStub(id, "T_MSG_ReadShort") ? Load32(from + 20) - length + random.Below(3) - 1
                                                                  : uint32_t(random.Next()));
                Store32(from + 32, uint32_t(random.Next()));
            }
        }
        else if (IsStub(id, "T_MSG_ReadFieldGap"))
        {
            ctx.r3.u64 = random.Below(8) ? 1 + random.Below(3) : random.Below(2) * 40;   // 0: read past the end
        }
        else if (IsStub(id, "T_MSG_EntityTypeFields"))
        {
            ctx.r3.u64 = g_entityType;
        }
        else if (IsStub(id, "T_Coop_ReadFrameHeader"))
        {
            ctx.r3.u64 = random.Below(8) ? 1 : 0x100;   // only the low byte counts
        }
        else if (IsStub(id, "T_NET_CompareAdr"))
        {
            ctx.r3.u64 = uint64_t(random.Below(2)) << 32 | random.Below(2);   // only the low half counts
        }
        else if (IsStub(id, "T_CL_FindSentOrigin"))
        {
            ctx.r3.u64 = random.Below(2);
        }
        else if (IsStub(id, "T_I_strncpyz"))
        {
            // A value, ending where the hook will look for its end.
            const uint32_t length = random.Below(8);
            for (uint32_t at = 0; at < length; at++) base[uint32_t(from + at)] = 'v';
            base[uint32_t(from + length)] = 0;
        }
        else if (IsStub(id, "T_LargeLocal_GetBuf"))
        {
            ctx.r3.u64 = kArenaBuffer;
        }
        else if (IsStub(id, "T_UI_ParseInfos"))
        {
            ctx.r3.u64 = random.Below(to + 1);   // what fitted of the room it was given
        }
        else if (IsStub(id, "__imp__T_UI_LoadArenas"))
        {
            // The title's own maps: a few, or so many that a custom map has no room.
            const uint32_t arenas = Named("T_DATA_UI_ArenaInfos") + 256;
            Store32(arenas, random.Below(8) ? random.Below(4) ? random.Below(20) : 60 + random.Below(6) : uint32_t(random.Next()));
        }
        else if (IsStub(id, "T_DB_FindXAssetHeader"))
        {
            ctx.r3.u64 = from == 24 && random.Below(16) ? Menu(random) : 0;
        }
        else if (IsStub(id, "__imp__T_Com_LoadUiFastFile"))
        {
            if (random.Below(8)) LoadUiZones(ctx, base, random);
        }
        else if (IsStub(id, "__imp__T_SEH_StringEd_GetString"))
        {
            if (random.Below(2)) ctx.r3.u64 = random.Below(4) ? 0 : 1ull << 32;   // only the low half counts
        }
        else if (IsStub(id, "__imp__T_DB_AuthLoadInit"))
        {
            ctx.r3.u64 = random.Below(2);
        }
        else if (IsStub(id, "__imp__T_MSG_ReadBitsCompress"))
        {
            ctx.r3.u64 = random.Below(100);
        }
    }

    // A string of the list where a hook will read it, as a pointer into the text
    // pages seldom lands on the start of one.
    uint32_t Plant(Random& random)
    {
        const uint32_t page = kText + 0x7000, at = random.Below(0xF00);
        auto& bytes = g_master[page];
        const char* text = kTexts[random.Below(std::size(kTexts))];
        std::memcpy(bytes.data() + at, text, std::strlen(text) + 1);
        g_changed.push_back(page);
        return page + at;
    }

    uint8_t* Held(uint32_t address)
    {
        const uint32_t page = address & ~(kPage - 1);
        auto& bytes = g_master[page];
        bytes.resize(kPage);
        g_changed.push_back(page);
        return bytes.data() + (address - page);
    }

    void Set32(uint32_t address, uint32_t value)
    {
        value = __builtin_bswap32(value);
        std::memcpy(Held(address), &value, 4);
    }

    void Set16(uint32_t address, uint32_t value)
    {
        value = __builtin_bswap32(value) >> 16;
        std::memcpy(Held(address), &value, 2);
    }

    // A string that does not need to be one of the list's.
    uint32_t PlantText(Random& random, const std::string& text)
    {
        const uint32_t address = kText + 0x7000 + random.Below(0x100);
        std::memcpy(Held(address), text.c_str(), text.size() + 1);
        return address;
    }

    uint32_t Get32(uint32_t address)
    {
        uint32_t value;
        std::memcpy(&value, g_master[address & ~(kPage - 1)].data() + (address & (kPage - 1)), 4);
        return __builtin_bswap32(value);
    }

    uint32_t Named(const char* name)
    {
        for (const Datum& datum : kNamed)
            if (!std::strcmp(datum.name, name))
                return datum.address;
        return 0;
    }

    uint32_t ArenaWord(Random& random) { return (kArena + random.Below(0x28000)) & ~3u; }

    // A table of `count` fields whose offsets stay in the state and whose flags and
    // kinds are the ones the readers tell apart.
    void FieldTable(Random& random, uint32_t table, uint32_t count)
    {
        for (uint32_t i = 0; i < count; i++)
        {
            Set32(table + i * 16 + 4, random.Below(0x40) * 4);
            Set32(table + i * 16 + 8, uint32_t(-85 + int32_t(random.Below(10))));
            *Held(table + i * 16 + 12) = uint8_t(random.Below(4));
        }
    }

    // cl_shownet's and the entity print's dvars: unset, or each value that prints.
    void Dvars(Random& random)
    {
        constexpr int32_t kValues[] = { -2, -1, 0, 1, 2, 3, 4 };
        const uint32_t shownet = ArenaWord(random);
        Set32(Named("T_DATA_ShownetDvar"), random.Below(4) ? shownet : 0);
        Set32(shownet + 12, uint32_t(kValues[random.Below(std::size(kValues))]));
        const uint32_t print = ArenaWord(random);
        Set32(Named("T_DATA_EntityPrintDvar"), print);
        *Held(print + 12) = uint8_t(random.Below(2));
    }

    uint32_t GuestText(Random& random, const std::string& text)
    {
        const uint32_t at = kArena + 0x10000 + random.Below(0x4000);
        for (size_t i = 0; i <= text.size(); i++) *Held(at + uint32_t(i)) = i < text.size() ? uint8_t(text[i]) : 0;
        return at;
    }

    // A print's format, of what the engine writes and of what it does not, and for
    // each %s of it a string or nothing.
    void Print(PPCContext& ctx, Random& random)
    {
        constexpr const char* kParts[] = { "%s", "%s", "%d", "%i", "%u", "%x", "%X", "%p", "%c", "%f", "%g", "%e", "%E", "%%",
                                           "%5.2f", "%-3d", "%l", "%q", "plain ", "\xE9\xFF", "%", "\n", "\r" };
        if (random.Below(32) == 0) { ctx.r4.u64 = 0; return; }
        std::string format;
        for (uint32_t parts = random.Below(10); parts > 0; parts--) format += kParts[random.Below(std::size(kParts))];
        ctx.r4.u64 = GuestText(random, format);
        const uint32_t text = kArena + 0x18000;
        constexpr const char* kTexts[] = { "", "a word", "line\n", "%s %d", "\xE9t\xE9" };
        const std::string chosen = kTexts[random.Below(std::size(kTexts))];
        for (size_t i = 0; i <= chosen.size(); i++) *Held(text + uint32_t(i)) = i < chosen.size() ? uint8_t(chosen[i]) : 0;
        PPCRegister* arguments[] = { &ctx.r5, &ctx.r6, &ctx.r7, &ctx.r8, &ctx.r9, &ctx.r10 };
        for (PPCRegister* reg : arguments)
            reg->u64 = random.Below(4) ? random.Next() : random.Below(2) ? text : 0;
        // What each argument is read as, the way the hook walks the format: a %s
        // must find text or nothing, anything else takes any value.
        size_t used = 0;
        for (size_t at = 0; at < format.size() && used < std::size(arguments); at++)
        {
            if (format[at] != '%') continue;
            if (++at < format.size() && format[at] == '%') continue;
            while (at < format.size() && !std::strchr("diuxXcsfgeEp", format[at])) at++;
            if (at >= format.size()) break;
            PPCRegister& reg = *arguments[used++];
            if (format[at] == 's' && reg.u32 != 0) reg.u64 = (reg.u64 & ~uint64_t(0xFFFFFFFF)) | text;
        }
    }

    void Tune(const Hook& hook, PPCContext& ctx, Random& random)
    {
        const std::string name = hook.name;
        if (name == "T_Com_Error" || name == "T_Com_Printf" || name == "T_Com_DPrintf" || name == "T_Com_PrintError" ||
            name == "T_Com_PrintWarning")
            Print(ctx, random);
        if (name == "T_Cbuf_AddText" || name == "T_DB_MissingAsset")
        {
            constexpr const char* kTexts[] = { "", "map mp_afghan\n", "set a b", "\xE9" };
            ctx.r4.u64 = random.Below(8) ? GuestText(random, kTexts[random.Below(4)]) : random.Below(2);
        }
        if (name == "T_MemFile_Read")
        {
            // Each read the hook knows by where it returns to, and now and then a
            // read of the wrong size from there, or from somewhere else.
            const char* kReturns[] = { "T_RET_Scr_ReadObjectType", "T_RET_Scr_ReadCodePos", "T_RET_Scr_ReadResumePos",
                                       "T_RET_G_ReadEntityPairCount", "T_RET_Scr_ReadObjectRef", "T_RET_Scr_ReadObjectCount" };
            constexpr uint32_t kSizes[] = { 1, 4, 4, 4, 2, 2 };
            const uint32_t kind = random.Below(std::size(kReturns));
            const uint32_t destination = ArenaWord(random);
            ctx.lr = uint64_t(random.Below(2)) << 32 | Named(kReturns[kind]);
            ctx.r4.u64 = random.Below(8) ? kSizes[kind] : kSizes[random.Below(std::size(kSizes))];
            ctx.r5.u64 = destination;
            if (random.Below(16) == 0)
                ctx.lr = Named(kReturns[kind]) + 4;
            const uint32_t start = kArena + random.Below(0x1000), end = start + random.Below(0x100);
            Set32(Named("T_DATA_ScrProgramStart"), random.Below(8) ? start : 0xFFFFFF00u);
            Set32(Named("T_DATA_ScrProgramEnd"), end);
            if (kind == 0)
                *Held(destination) = uint8_t(random.Below(2) ? 24 * 8 + random.Below(32) : random.Below(256));
            else if (kind == 1 || kind == 2)
                Set32(destination, random.Below(4) ? end - start - 2 + random.Below(4) : uint32_t(random.Next()));
            else if (kind == 3)
                Set32(destination, 254 + random.Below(5));
            else if (kind == 4)
                Set16(destination, random.Below(2) ? random.Below(4) : 0x7FFE + random.Below(4));
            else
                Set16(destination, 36862 + random.Below(5));
        }
        if (name == "T_SEH_StringEd_GetString" && random.Below(2))
        {
            // Now and then "Dust" with the rest of an arena's "Dust\0II" after its end,
            // which only a text that stops at its zero tells from it.
            const std::string text = random.Below(8) == 0 ? std::string("Dust\0II", 7)
                                     : random.Below(8)    ? kMapTexts[random.Below(std::size(kMapTexts))] : "";
            ctx.r3.u64 = PlantText(random, text);
        }
        if (name == "T_Scr_ResolveObjectRef")
            ctx.r3.u64 = (ctx.r3.u64 & ~uint64_t(0xFFFF)) | (36862 + random.Below(5));
        if (name == "T_R_VideoSizes")
        {
            ctx.r3.u64 = ArenaWord(random);
            *Held(ctx.r3.u32 + 1) = uint8_t(random.Below(2));
        }
        if (name == "T_R_SetVidConfig" || name == "T_R_DrawStretchPic")
        {
            // Sizes as the title would write them, and now and then none.
            const uint32_t config = Named("T_DATA_VidConfig");
            constexpr uint32_t kSizes[] = { 0, 640, 1024, 1280, 720, 600, 800 };
            for (uint32_t field = 0; field < 16; field += 4)
                Set32(config + field, random.Below(8) ? kSizes[1 + random.Below(6)] : kSizes[0]);
            constexpr float kAspects[] = { 16.0f / 9.0f, 1.6f, 4.0f / 3.0f, 2.0f };
            uint32_t aspect;
            std::memcpy(&aspect, &kAspects[random.Below(4)], 4);
            Set32(config + 28, aspect);
        }
        if (name == "T_R_DrawStretchPic")
        {
            // A picture over the whole frame, with each edge either side of the half
            // pixel, of a material whose textures may or may not all be files.
            const uint32_t config = Named("T_DATA_VidConfig");
            const uint8_t* size = reinterpret_cast<const uint8_t*>(Held(config + 8));
            const double width = (size[0] << 24 | size[1] << 16 | size[2] << 8 | size[3]);
            const double height = (size[4] << 24 | size[5] << 16 | size[6] << 8 | size[7]);
            const auto near = [&](double value) { return value + (double(random.Below(5)) - 2.0) * 0.5; };
            ctx.f1.f64 = near(0.0);
            ctx.f2.f64 = near(0.0);
            ctx.f3.f64 = near(width);
            ctx.f4.f64 = near(height);
            // In double precision as a register holds them, past what a float can.
            for (PPCRegister* s : { &ctx.f5, &ctx.f6, &ctx.f7, &ctx.f8 })
                s->f64 = random.Below(2) ? double(random.Below(1024)) / 1024.0 : double(random.Next() >> 11) / 9007199254740992.0;
            const uint32_t material = random.Below(8) ? ArenaWord(random) : 0;
            Set32(ctx.r1.u32 + 92, material);
            // No material, with one at address 0 for a hook that forgot to ask.
            {
                const uint32_t count = material ? random.Below(4) : 1 + random.Below(3), table = random.Below(8) || !material ? ArenaWord(random) : 0;
                *Held(material + 57) = uint8_t(count);
                Set32(material + 68, table);
                for (uint32_t i = 0; i < count && table; i++)
                {
                    const uint32_t image = random.Below(16) || !material ? ArenaWord(random) : 0;
                    Set32(table + i * 12 + 8, image);
                    if (image) *Held(image + 58) = uint8_t(random.Below(8) || !material ? 3 : 2);
                }
                Set32(material, random.Below(4) ? PlantText(random, std::string(random.Below(80), 'm')) : 0);
            }
        }
        if (name == "T_UI_EvaluateStatement")
        {
            // A statement of entries, some the two forms the hook looks for, around a
            // dvarbool of a list of dvars, `widescreen` in any case or something else.
            const bool again = random.Below(4) == 0;
            const uint32_t statement = ArenaWord(random), entries = again ? kArena + 0x3000 : kArena + 0x4000 + 4 * random.Below(0x100);
            const uint32_t data = ArenaWord(random), list = ArenaWord(random), dvar = ArenaWord(random);
            uint32_t at = 0;
            const auto put = [&](uint32_t kind, uint32_t second, uint32_t third) {
                Set32(entries + at * 12, kind);
                Set32(entries + at * 12 + 4, second);
                Set32(entries + at * 12 + 8, third);
                at++;
            };
            const auto constant = [&] {
                constexpr uint32_t kBits[] = { 0x44555555u, 0x3F800000u, 0xFFC00000u, 0x80000000u };
                if (random.Below(8) == 0) put(0, 1, kBits[random.Below(4)]);        // not an operand
                else if (random.Below(2)) put(1, random.Below(8) ? 1 : 3, kBits[random.Below(4)]);
                else put(1, 0, random.Below(2) ? 213 + random.Below(3) : 0u - 213 - random.Below(3));
            };
            const auto test = [&] {
                put(random.Below(8) ? 0 : 1, random.Below(8) ? 24 : 23, 0);
                put(random.Below(8) ? 1 : 0, random.Below(8) ? 0 : 1, random.Below(2));
                put(0, random.Below(8) ? 1 : 3, 0);
            };
            // Now and then the one place again, as an integer holding the bits the hook
            // made of it last time: a zone loaded over it.
            if (again)
            {
                const float scaled = 213.0f * [] {
                    const char* asked = std::getenv("MW2_RENDER");
                    unsigned width = 0, height = 0;
                    if (!asked || std::sscanf(asked, "%ux%u", &width, &height) != 2 || width * 3 <= height * 4) return 1.0f;
                    return (float(width) / float(height) - 4.0f / 3.0f) / (16.0f / 9.0f - 4.0f / 3.0f);
                }();
                uint32_t bits;
                std::memcpy(&bits, &scaled, 4);
                put(1, random.Below(2) ? 0 : 1, random.Below(2) ? 213 : bits);
                put(0, 2, 0);
                test();
            }
            for (uint32_t forms = again ? 0 : 1 + random.Below(3); forms > 0; forms--)
            {
                if (random.Below(2)) { constant(); put(0, random.Below(8) ? 2 : 3, 0); test(); }
                else { test(); put(0, random.Below(8) ? 2 : 3, 0); constant(); }
                if (random.Below(2)) put(0, 4, 0);
            }
            Set32(statement, random.Below(16) ? at : at - random.Below(4));
            Set32(statement + 4, entries);
            Set32(statement + 8, data);
            Set32(data + 12, list);
            Set32(list, random.Below(8) ? dvar : 0);
            Set32(list + 4, dvar);
            constexpr const char* kNames[] = { "widescreen", "WideScreen", "widescreens", "widescree", "other" };
            Set32(dvar + 4, random.Below(8) ? PlantText(random, kNames[random.Below(5)]) : 0);
            // No name, with `widescreen` at address 0 for a hook that forgot to ask.
            std::memcpy(Held(0), "widescreen", 11);
            ctx.r4.u64 = statement;
        }
        if (name == "T_NET_OutOfBandPrint" && random.Below(16) == 0)
        {
            // No text, with the format at address 0 for a hook that forgot to ask.
            ctx.r6.u64 = 0;
            std::memcpy(Held(0), "error\nEXE_SERVER_IS_DIFFERENT_VER\x15", 34);
        }
        else if (name == "T_NET_OutOfBandPrint" && random.Below(8))
        {
            // The format, whole, cut at any length, or a byte off anywhere in it.
            static const std::string kDifferent = "error\nEXE_SERVER_IS_DIFFERENT_VER\x15";
            std::string text = kDifferent + "1.4\n";
            switch (random.Below(4))
            {
            case 0: text.resize(random.Below(uint32_t(kDifferent.size()))); break;
            case 1: text[random.Below(uint32_t(kDifferent.size()))] ^= 0x20; break;
            default: break;
            }
            ctx.r6.u64 = PlantText(random, text);
        }
        if (name == "T_UI_ReplaceDirective")
        {
            // Directives, with names either side of what the buffer takes.
            constexpr const char* kParts[] = { "[{", "}]", "abc", "[", "]", "{", "}", "x" };
            std::string text;
            for (uint32_t part = random.Below(8); part > 0; part--)
                text += kParts[random.Below(std::size(kParts))];
            if (random.Below(4) == 0)
                text += "[{" + std::string(253 + random.Below(6), 'n') + "}]" + kParts[random.Below(std::size(kParts))];
            ctx.r4.u64 = PlantText(random, text);
            ctx.r5.u64 = ArenaWord(random);
            ctx.r6.u64 = random.Below(8) ? 1 + random.Below(32) : uint32_t(int32_t(random.Below(3)) - 1);
        }
        if (name == "T_CL_PacketEvent" || name == "T_CL_ConnectionlessPacket")
        {
            // Per client, a connection state, a connection, and a command to tell apart.
            constexpr const char* kCommands[] = { "challengeResponse", "CHALLENGERESPONSE", "connectResponse", "statResponse",
                                                  "mstart", "MHEAD", "mdata", "mstate", "mstatex", "mstar", "ping", "" };
            constexpr uint32_t kStates[] = { 3, 4, 5, 6, 7 };
            const uint32_t client = random.Below(2);
            ctx.r3.u64 = client;
            Set32(Named("T_DATA_ClientConnState") + client * 3216, kStates[random.Below(std::size(kStates))]);
            Set32(Named("T_DATA_ClientConnections"), ArenaWord(random));
            const uint32_t msg = ArenaWord(random), data = ArenaWord(random);
            ctx.r6.u64 = msg;
            Set32(msg + 20, 2 + random.Below(4));
            Set32(msg + 8, data);
            Set32(data, random.Below(2) ? 0xFFFFFFFFu : 0xFFFFFFFEu);
            if (name == "T_CL_ConnectionlessPacket")
            {
                const uint32_t args = Named("T_DATA_CmdArgs"), nest = random.Below(3), argv = ArenaWord(random);
                Set32(args, nest);
                Set32(args + 68 + nest * 4, uint32_t(int32_t(random.Below(3)) - 1));
                Set32(args + 100 + nest * 4, argv);
                Set32(argv, random.Below(8) ? PlantText(random, kCommands[random.Below(std::size(kCommands))]) : 0);
            }
        }
        if (name == "T_Coop_ReceiveFrames")
        {
            *Held(0x83A74289u) = random.Below(8) == 0;
            ctx.r3.u64 = ArenaWord(random);
            Set32(ctx.r3.u32 + 20, random.Below(0x4000));
        }
        if (name == "T_CoopLaunch" || name == "T_CoopAccept" || name == "T_CoopHeartbeat")
            ctx.r6.u64 = ArenaWord(random);
        if (name == "T_MSG_ReadDeltaHudElems" || name == "T_MSG_ReadDeltaPlayerState")
        {
            const uint32_t fields = 38 + random.Below(8);
            Set32(Named("T_DATA_HudElemFieldCount"), fields);
            FieldTable(random, Named("T_DATA_HudElemFields"), fields);
        }
        if (name == "T_MSG_ReadDeltaHudElems")
        {
            ctx.r3.u64 = ArenaWord(random);
            ctx.r5.u64 = ArenaWord(random);
            ctx.r6.u64 = ArenaWord(random);
            ctx.r7.u64 = random.Below(40);
        }
        if (name == "T_MSG_ReadDeltaFields")
        {
            const uint32_t fields = 1 + random.Below(12);
            ctx.r3.u64 = ArenaWord(random);
            ctx.r5.u64 = ArenaWord(random);
            ctx.r6.u64 = ArenaWord(random);
            ctx.r7.u64 = random.Below(8) ? fields : 0;
            ctx.r8.u64 = ArenaWord(random);
            FieldTable(random, ctx.r8.u32, fields + 2);
            Dvars(random);
        }
        if (name == "T_MSG_ReadDeltaEntity")
        {
            const uint32_t fields = 1 + random.Below(12), table = ArenaWord(random);
            ctx.r3.u64 = ArenaWord(random);
            ctx.r5.u64 = ArenaWord(random);
            ctx.r6.u64 = ArenaWord(random);
            Set32(Named("T_DATA_EntityFieldCount"), fields);
            FieldTable(random, Named("T_DATA_ETypeField"), 1);
            FieldTable(random, table, fields + 2);
            g_entityType = ArenaWord(random);
            Set32(g_entityType, table);
            Set32(g_entityType + 4, random.Below(4) ? fields : random.Below(14));
            Set32(ctx.r5.u32 + 4, Get32(ctx.r6.u32 + 4) + (random.Below(2) ? 0 : 1));
            Dvars(random);
        }
        if (name == "T_MSG_ReadDeltaPlayerState")
        {
            const uint32_t fields = 1 + random.Below(12);
            ctx.r3.u64 = random.Below(3);
            ctx.r4.u64 = ArenaWord(random);
            ctx.r6.u64 = random.Below(4) ? ArenaWord(random) : 0;
            ctx.r7.u64 = ArenaWord(random);
            Set32(Named("T_DATA_PlayerStateFieldCount"), fields);
            FieldTable(random, Named("T_DATA_PlayerStateFields"), fields + 2);
            const uint32_t objectives = 1 + random.Below(4);
            Set32(Named("T_DATA_ObjectiveFieldCount"), objectives);
            FieldTable(random, Named("T_DATA_ObjectiveFields"), objectives + 2);
            Dvars(random);
        }
        if (name == "T_SV_UpdateServerCommandsToClient" || name == "T_SV_ResendReliableCommands" ||
            name == "T_SV_DumpPendingReliableCommands")
        {
            // Either side of the engine's own limit on what is pending.
            const uint32_t client = (kArena + random.Below(0x28000)) & ~3u, sequence = Word(random);
            ctx.r3.u64 = client;
            Set32(client + 0x20E50, sequence);
            Set32(client + 0x20E54, sequence - (126 + random.Below(5)));
        }
        if (name == "T_SV_CheckCommandTime")
        {
            const uint32_t command = (kArena + random.Below(0x28000)) & ~3u;
            ctx.r4.u64 = command;
            Set32(command, Get32(Named("T_DATA_ServerTime")) + 19998 + random.Below(5));
        }
        if (name == "T_Party_Frame" || name == "T_UI_RunMenuScript" || name == "T_Coop_LoadMap")
        {
            // A name to cut, at a stack either side of what the cut will take.
            const uint32_t dvar = (kArena + random.Below(0x28000)) & ~3u;
            Set32(Named("T_DATA_MapNameDvar"), random.Below(8) ? dvar : 0);
            Set32(dvar + 12, random.Below(8) ? Plant(random) : 0);
            if (random.Below(2))
                ctx.r1.u64 = (0x1000 + random.Below(0x2000)) & ~0xFu;
        }
        if (name == "T_G_LoadMigratedGame")
            Set32(Named("T_DATA_MigrationHeader") + 1116, 0x180000 - 2 + random.Below(5));
        if (name == "T_MemFile_BeginSegment")
        {
            // A chain of segments that adds up, with now and then a size that does not.
            const uint32_t file = (kArena + random.Below(0x20000)) & ~3u, data = (kArena + random.Below(0x20000)) & ~3u;
            const uint32_t segments = random.Below(5);
            uint32_t offset = 0;
            for (uint32_t i = 0; i <= segments; i++)
            {
                // Only the last can be short: the next word would be written over it.
                const uint32_t size = i < segments || random.Below(2) ? 4 + random.Below(40) : random.Below(5);
                Set32(data + offset, size);
                offset += size ? size : 4;
            }
            Set32(file, data);
            Set32(file + 4, offset + random.Below(8) - 2);   // cut short, or with room left after
            g_master[(file + 29) & ~(kPage - 1)][(file + 29) & (kPage - 1)] = random.Below(4) == 0;
            g_changed.push_back((file + 29) & ~(kPage - 1));
            ctx.r3.u64 = file;
            ctx.r4.u64 = random.Below(8) ? segments : uint32_t(-1);
        }
        if (name == "T_GlassState_Parse")
        {
            const uint32_t msg = (kArena + random.Below(0x28000)) & ~3u;
            ctx.r3.u64 = msg;
            Set32(msg, random.Below(4) == 0);
        }
        if (name == "T_CL_ParsePacketEntities" || name == "T_CL_DeltaEntity")
        {
            // A baseline at either side of what one can hold.
            const uint32_t frame = (kArena + random.Below(0x28000)) & ~3u;
            const uint32_t msg = (kArena + random.Below(0x28000)) & ~3u;
            const uint32_t entities = 1022 + random.Below(5);
            (name == "T_CL_DeltaEntity" ? ctx.r6 : ctx.r7).u64 = frame;
            ctx.r4.u64 = msg;
            Set32(frame + 12676, 8);
            Set32(frame + 12700, entities);
            Set32(Named("T_DATA_BaselineEntityCount"), entities);
            Set32(msg, random.Below(2));
        }
        if (name == "T_D3D_Present" || name == "T_D3D_ArenaWait")
        {
            // A device in the arena, now and then with what is left of a 64-bit value.
            const uint32_t device = ArenaWord(random) & 0xFFFF0u;
            ctx.r3.u64 = kArena + device;
            if (random.Below(8) == 0) ctx.r3.u64 |= random.Next() << 32;
            if (name == "T_D3D_ArenaWait")
            {
                // The progress block's pointer: none, one of a few (so that the block is
                // seen again as often as it changes), or anything.
                constexpr uint32_t kBlocks[] = { 0, 0xA0001000u, 0xA0001000u, 0xBFFFFFF0u, 0xC0002000u, 0x82000004u, 0x40000000u };
                Set32(kArena + device + 10896, random.Below(8) ? kBlocks[random.Below(std::size(kBlocks))] : uint32_t(random.Next()));
            }
        }
        if (name == "T_D3D_ReplayRecording")
        {
            // A recording's chunk list: next at +0, a count at +4. Mostly a list that ends,
            // now and then with a count of 0 or past 2^20 or a cycle. D3D9 warns of 8 at
            // most, and nothing after them is seen, so they are rare enough here that the
            // lists at the chunk bound come before them.
            const uint32_t object = ArenaWord(random);
            ctx.r4.u64 = random.Below(16) ? object : 0;
            // The hare reaches the end in half the chunks, so the 4096 the tortoise may
            // walk is a list of about 8192: 8193 and 8195 are either side of the bound.
            constexpr uint32_t kLengths[] = { 0, 1, 2, 3, 5, 17, 100, 4096, 4097, 8192, 8193, 8194, 8195, 8196 };
            const uint32_t length = kLengths[random.Below(std::size(kLengths))];
            const uint32_t nodes = kArena + 0x8000, stride = length > 4100 ? 8 : 8 + 4 * random.Below(3);
            for (uint32_t i = 0; i < length; i++)
            {
                const uint32_t node = nodes + i * stride;
                Set32(node, i + 1 < length ? node + stride : 0);
                Set32(node + 4, random.Below(16) ? 1 + random.Below(64) : (1u << 20) - random.Below(2));
            }
            if (length && random.Below(64) == 0)
                Set32(nodes + random.Below(length) * stride + 4, random.Below(2) ? 0 : (1u << 20) + 1);
            if (length && random.Below(64) == 0)
                Set32(nodes + (length - 1) * stride, nodes + random.Below(length) * stride);   // a cycle
            Set32(object + 116, length ? nodes : 0);
        }
        if (name == "T_CG_SetClientDvarFromServer" && random.Below(8) == 0)
        {
            // Address 0 is memory here, and only the null check keeps it from a name.
            const char* text = random.Below(2) ? "nextmap" : "com_errorResolveCommand";
            std::memcpy(g_master[0].data(), text, std::strlen(text) + 1);
            g_changed.push_back(0);
            ctx.r4.u64 = 0;
        }
        if (name == "T_Dvar_RegisterInt" && random.Below(2))
        {
            ctx.r3.u64 = Plant(random);
            ctx.r5.u64 = random.Below(10);   // the minimum and the maximum
            ctx.r6.u64 = random.Below(20);
        }
        if (name == "T_CG_SetClientDvarFromServer" && random.Below(2))
            ctx.r4.u64 = Plant(random);
        if (name == "T_MSG_ReadBitsCompress" || name == "T_SV_ParseClientMessage" || name == "T_CL_ParseServerMessage")
        {
            constexpr uint32_t kSizes[] = { 0, 1, 16, 255, 1024, 2047, 2048, 2049, 3000, 0xFFFF, 0x10000, 0x10001, 80000, 0xFFFFFFFF, 0x80000000 };
            ctx.r3.u64 = kArena + random.Below(0x1000);
            ctx.r4.u64 = kArena + 0x1000 + random.Below(0x1000);
            // The sizes that reach 64 KB take 50000 calls: a few of those will do.
            ctx.r5.u64 = kSizes[random.Below(random.Below(16) ? 9 : std::size(kSizes))];
        }
        else if (name == "T_Playlist_UnplayHistory")
        {
            // A count of 2^31 would store over 2 GB of the guest, and every page of
            // it would be kept twice to compare. 0x10000 walks past any table.
            ctx.r4.u64 = kArena + random.Below(0x28000);
            ctx.r5.u64 = random.Below(8) ? random.Below(300) : (random.Below(2) ? 0x10000 : 0xFFFFFFFFu - random.Below(4));
        }
    }

    std::string TakeLog()
    {
        std::fflush(stderr);
        const off_t size = lseek(g_log, 0, SEEK_CUR);
        std::string text(size, 0);
        pread(g_log, text.data(), size, 0);
        lseek(g_log, 0, SEEK_SET);
        ftruncate(g_log, 0);
        return text;
    }

    struct Outcome
    {
        PPCContext ctx;
        Pages written;
        std::vector<Call> calls;
        std::string log;
    };

    Outcome Run(const Hook& hook, const PPCContext& start, int side)
    {
        g_side = side;
        g_calls.clear();
        Load();
        mw2_rig_reset_heap();
        Outcome outcome;
        std::memcpy(&outcome.ctx, &start, sizeof start);
        if (!setjmp(g_escape))
            hook.side[side](outcome.ctx, g_base);
        outcome.written = Harvest();
        outcome.calls = g_calls;
        outcome.log = TakeLog();
        return outcome;
    }

    // Where the two differ, or nothing.
    std::string Difference(const Outcome& cpp, const Outcome& rust)
    {
        char text[160];
        if (cpp.log != rust.log)
            return "log:\n  C++:  " + cpp.log + "  Rust: " + rust.log;
        if (cpp.calls != rust.calls)
        {
            size_t i = 0;
            while (i < cpp.calls.size() && i < rust.calls.size() && cpp.calls[i] == rust.calls[i]) i++;
            const auto name = [](const std::vector<Call>& calls, size_t at) {
                return at < calls.size() ? kStubs[calls[at].stub].name : "nothing";
            };
            std::snprintf(text, sizeof text, "call %zu: C++ %s, Rust %s", i, name(cpp.calls, i), name(rust.calls, i));
            return text;
        }
        if (std::memcmp(&cpp.ctx, &rust.ctx, sizeof cpp.ctx))
        {
            const auto* a = reinterpret_cast<const uint8_t*>(&cpp.ctx);
            const auto* b = reinterpret_cast<const uint8_t*>(&rust.ctx);
            size_t at = 0;
            while (a[at] == b[at]) at++;
            std::snprintf(text, sizeof text, "context byte %zu", at);
            return text;
        }
        for (const Pages* pages : { &cpp.written, &rust.written })
            for (const auto& [page, unused] : *pages)
            {
                const auto a = cpp.written.find(page), b = rust.written.find(page);
                const uint8_t* x = a == cpp.written.end() ? Pristine(page) : a->second.data();
                const uint8_t* y = b == rust.written.end() ? Pristine(page) : b->second.data();
                for (uint32_t at = 0; at < kPage; at++)
                    if (x[at] != y[at])
                    {
                        std::snprintf(text, sizeof text, "memory at %llX: C++ %02X, Rust %02X", (unsigned long long)(page + at), x[at], y[at]);
                        return text;
                    }
            }
        return {};
    }
}

// guest::Base() is guest_memory.h's, through kernel.h: mw2_guest_base, which main sets.
extern "C" bool mw2_stutters_on();

// hook_diff <seed> <cases> [hook[,hook...]]
extern "C" uint8_t* mw2_guest_base;

// What src/runtime/bridge.rs asks of the C++ runtime, which is not here.
extern "C" uint64_t mw2_kernel_system_time() { return 0x01DD000000000000ull; }
extern "C" void mw2_watch_costs(void*) {}
extern "C" bool mw2_watch_handle_fault(const void*) { return false; }
extern "C" uint64_t mw2_gpu_frame_count() { return 0; }
extern "C" void mw2_crash_on_fault() {}
// And what the kernel's objects name, which the custom maps' heap brings in.
extern "C" void mw2_crash_request_exit(const char*) { std::abort(); }
extern "C" void mw2_gpu_set_ring_buffer(uint32_t, uint32_t) { std::abort(); }
extern "C" void mw2_gpu_set_read_pointer_write_back(uint32_t, uint32_t) { std::abort(); }
extern "C" void mw2_gpu_set_interrupt_callback(uint32_t, uint32_t) { std::abort(); }
extern "C" void mw2_gpu_swap() { std::abort(); }
extern "C" void mw2_renderer_flash_seen(const char*) { std::abort(); }

// What the renderer's hooks call of the renderer: loggers, the same text whichever side
// called, as the calls are part of what a hook does. The C++ hooks call the C++
// renderer's functions; the Rust ones what src/runtime/bridge.rs declares of
// runtime/rust_calls.cpp.
namespace
{
    void Logged(const char* what, uint64_t a = 0, uint64_t b = 0)
    {
        std::fprintf(stderr, "[renderer] %s %llx %llx\n", what, (unsigned long long)a, (unsigned long long)b);
        std::fflush(stderr);
    }
}

namespace gpu
{
    void Presented() { Logged("present"); }
    // Values, which gpu.rs gives its Rust callers too (gpu::rig).
    uint64_t FrameCount() { return 0; }
    bool Consuming() { return false; }
}
namespace vk::renderer
{
    void BeforeStreamWrite(uint32_t physical, uint32_t size) { Logged("before stream write", physical, size); }
    void PrepareShader(bool pixel, const uint32_t* code, size_t words)
    {
        Logged("prepare shader", pixel, words ? code[0] ^ code[words - 1] ^ words : 0);
    }
}
namespace vk::textures
{
    void MemoryWritten(uint32_t physical, uint32_t size) { Logged("memory written", physical, size); }
}
// The GPU capture's state, which arena.cpp reads (never capturing here).
namespace gpu::stream::detail
{
    bool capturing = false, replaying = false, counting = false;
}
extern "C" void mw2_census_present() { Logged("present"); }
extern "C" void mw2_renderer_before_stream_write(uint32_t physical, uint32_t size) { Logged("before stream write", physical, size); }
extern "C" void mw2_renderer_prepare_shader(bool pixel, const uint32_t* code, size_t words)
{
    Logged("prepare shader", pixel, words ? code[0] ^ code[words - 1] ^ words : 0);
}
extern "C" void mw2_textures_memory_written(uint32_t physical, uint32_t size) { Logged("memory written", physical, size); }

int main(int argc, char** argv)
{
    const uint64_t seed = std::strtoull(argv[1], nullptr, 10);
    const uint64_t cases = std::strtoull(argv[2], nullptr, 10);
    const char* only = argc > 3 ? argv[3] : nullptr;

    g_report = fdopen(dup(2), "w");
    g_log = memfd_create("log", 0);
    dup2(g_log, 2);
    // MW2_STUTTERS is read once, with a warning off the console's 60: before the cases,
    // not in whichever side asks first.
    mw2_stutters_on();
    TakeLog();

    g_base = static_cast<uint8_t*>(mmap(nullptr, kMapped + 0x20000, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0));
    g_base = reinterpret_cast<uint8_t*>((reinterpret_cast<uintptr_t>(g_base) + 0xFFFF) & ~uintptr_t(0xFFFF));
    mw2_guest_base = g_base;

    struct sigaction action{};
    action.sa_sigaction = Fault;
    action.sa_flags = SA_SIGINFO;
    sigaction(SIGSEGV, &action, nullptr);

    g_pages.push_back(0);
    for (uint32_t page = kArena; page < kArena + kArenaSize; page += kPage) g_pages.push_back(page);
    for (const uint32_t address : kData)
        for (uint32_t page = address & ~(kPage - 1); page <= ((address + 512) & ~(kPage - 1)); page += kPage)
            if (std::find(g_pages.begin(), g_pages.end(), page) == g_pages.end())
                g_pages.push_back(page);

    // The game folder tools/hook_diff.py made for this seed, which both sides list.
    if (const char* game = std::getenv("HOOK_DIFF_GAME"))
    {
        kernel::GameRootKept() = game;
        mw2_rig_set_game_root(game);
    }

    g_dirty.reserve(kMostPages);   // a signal handler must not allocate
    std::vector<const Hook*> hooks;
    // `only` is one hook or several, separated by commas.
    const auto chosen = [&](const char* name) {
        if (!only) return true;
        const size_t length = std::strlen(name);
        for (const char* at = only; (at = std::strstr(at, name)); at += length)
            if ((at == only || at[-1] == ',') && (at[length] == ',' || at[length] == 0)) return true;
        return false;
    };
    for (const Hook& hook : kHooks)
        if (chosen(hook.name))
            hooks.push_back(&hook);
    if (hooks.empty())
    {
        std::fprintf(g_report, "no hook %s here\n", only);
        return 2;
    }

    std::map<std::string, uint64_t> ran, logged, called;
    Random random{ seed };
    for (uint64_t i = 0; i < cases; i++)
    {
        const Hook& hook = *hooks[random.Below(uint32_t(hooks.size()))];
        Fill(random, i == 0);

        PPCContext start;
        auto* bytes = reinterpret_cast<uint8_t*>(&start);
        for (size_t at = 0; at < sizeof start; at++) bytes[at] = uint8_t(random.Next());
        PPCRegister* arguments[] = { &start.r3, &start.r4, &start.r5, &start.r6, &start.r7, &start.r8, &start.r9, &start.r10 };
        for (PPCRegister* reg : arguments)
        {
            reg->u64 = Word(random);
            // What is left of a 64-bit value: a hook that takes the low half must not see it.
            if (random.Below(4) == 0)
                reg->u64 |= random.Next() << 32;
        }
        start.r1.u64 = random.Below(32) ? kStack + 0x8000 - 0x100 * random.Below(16) * 8 : random.Below(0x4000);
        start.r1.u64 &= ~0xFull;
        start.f1.f64 = random.Below(4) ? double(int32_t(random.Next())) / 256.0 : (random.Below(2) ? __builtin_nan("") : 170.0);
        Tune(hook, start, random);

        g_running = hook.name;
        const Outcome cpp = Run(hook, start, 0);
        const Outcome rust = Run(hook, start, 1);
        // Only a case whose hook says it patched the menu: the page holds other
        // things, zone lists and noise, the rest of the time.
        if (const auto page = cpp.written.find(kMenu);
            page != cpp.written.end() && cpp.log.find("in the bonus maps menu") != std::string::npos)
        {
            uint32_t items;
            std::memcpy(&items, page->second.data() + 296, 4);
            items = __builtin_bswap32(items);
            if (items >= 0x84600000u && items < 0xA0000000u) g_lastMade = items;   // the kernel's heap
        }
        if (std::getenv("HOOK_DIFF_SHOW_LOG") && !cpp.log.empty())
            std::fprintf(g_report, "%s case %llu:\n%s", hook.name, (unsigned long long)i, cpp.log.c_str());
        ran[hook.name]++;
        logged[hook.name] += !cpp.log.empty();
        called[hook.name] += cpp.calls.size();
        const std::string difference = Difference(cpp, rust);
        if (!difference.empty())
        {
            std::fprintf(g_report, "DIFFERENT: %s, seed %llu, case %llu: %s\n", hook.name, (unsigned long long)seed,
                         (unsigned long long)i, difference.c_str());
            return 1;
        }
    }
    for (const auto& [name, count] : ran)
        std::fprintf(g_report, "same: %-36s %8llu cases, %6llu logged, %9llu calls\n", name.c_str(), (unsigned long long)count,
                     (unsigned long long)logged[name], (unsigned long long)called[name]);
    return 0;
}
