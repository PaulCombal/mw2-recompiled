// The title's presence line (presence.h).
#include "presence.h"
#include "guest.h"
#include "log.h"
#include "title.h"

#include <cstdlib>
#include <cstring>
#include <map>
#include <mutex>
#include <vector>

#if defined(MW2_SCREENS) && defined(T_DATA_Spa)
#include <zlib.h>
#endif

namespace
{
    constexpr uint32_t kUsers = 4;
    constexpr uint32_t X_CONTEXT_PRESENCE = 0x00008001;

    std::mutex g_lock;
    std::map<uint32_t, uint32_t> g_contexts[kUsers];

    // What the XLAST says, in English.
    struct Context
    {
        uint32_t fallback = 0;                 // its defaultValue
        std::map<uint32_t, uint32_t> strings;  // value -> string id
    };
    struct Strings
    {
        bool loaded = false;
        std::map<uint32_t, std::string> text;  // string id -> English
        std::map<uint32_t, uint32_t> modes;    // presence mode -> string id
        std::map<uint32_t, Context> contexts;  // context id -> its values
    };
    Strings g_strings;

    // An attribute of the tag starting at `at`, or "".
    std::string Attribute(const std::string& xml, size_t at, const char* name)
    {
        const size_t end = xml.find('>', at);
        const std::string key = std::string(" ") + name + "=\"";
        const size_t found = xml.find(key, at);
        if (found == std::string::npos || found > end) return {};
        const size_t from = found + key.size();
        return xml.substr(from, xml.find('"', from) - from);
    }

    uint32_t Number(const std::string& text) { return uint32_t(std::strtoul(text.c_str(), nullptr, 0)); }

    // The XML's five entities, the line's own spaces trimmed, and the
    // registered mark the fonts do not have.
    std::string Plain(std::string text)
    {
        static const std::pair<const char*, const char*> kEntities[] = {
            { "&lt;", "<" }, { "&gt;", ">" }, { "&quot;", "\"" }, { "&apos;", "'" }, { "\xC2\xAE", "" }, { "&amp;", "&" } };
        for (const auto& [from, to] : kEntities)
            for (size_t at; (at = text.find(from)) != std::string::npos;) text.replace(at, std::strlen(from), to);
        while (!text.empty() && std::strchr(" \r\n\t", text.back())) text.pop_back();
        return text;
    }

    std::string Utf8(const uint8_t* data, size_t size)
    {
        std::string out;
        for (size_t i = 0; i + 1 < size; i += 2)
        {
            uint32_t c = data[i] | (data[i + 1] << 8);
            if (c >= 0xD800 && c < 0xDC00 && i + 3 < size)
            {
                c = 0x10000 + ((c - 0xD800) << 10) + ((data[i + 2] | (data[i + 3] << 8)) - 0xDC00);
                i += 2;
            }
            if (c < 0x80) out += char(c);
            else if (c < 0x800) { out += char(0xC0 | (c >> 6)); out += char(0x80 | (c & 0x3F)); }
            else if (c < 0x10000)
            {
                out += char(0xE0 | (c >> 12)); out += char(0x80 | ((c >> 6) & 0x3F)); out += char(0x80 | (c & 0x3F));
            }
            else
            {
                out += char(0xF0 | (c >> 18)); out += char(0x80 | ((c >> 12) & 0x3F));
                out += char(0x80 | ((c >> 6) & 0x3F)); out += char(0x80 | (c & 0x3F));
            }
        }
        return out;
    }

    uint32_t Be32(const uint8_t* p) { return uint32_t(p[0]) << 24 | p[1] << 16 | p[2] << 8 | p[3]; }

    // The XLAST's text: an XDBF whose metadata entry XSRC holds its name, then
    // its size, then gzip.
    std::string Xlast()
    {
#if defined(MW2_SCREENS) && defined(T_DATA_Spa)
        const uint8_t* spa = GuestPtr<uint8_t>(T_DATA_Spa);
        const uint32_t size = T_DATA_SpaSize;
        if (!spa || std::memcmp(spa, "XDBF", 4) != 0) return {};
        const uint32_t entriesMax = Be32(spa + 8), entries = Be32(spa + 12), freeMax = Be32(spa + 16);
        const size_t base = 24 + size_t(entriesMax) * 18 + size_t(freeMax) * 8;
        for (uint32_t i = 0; i < entries && 24 + (i + 1) * 18 <= size; i++)
        {
            const uint8_t* entry = spa + 24 + i * 18;
            const uint16_t space = uint16_t(entry[0] << 8 | entry[1]);
            if (space != 1 || std::memcmp(entry + 6, "XSRC", 4) != 0 || Be32(entry + 2) != 0) continue;
            const size_t at = base + Be32(entry + 10), length = Be32(entry + 14);
            if (at + length > size || length < 16) return {};
            const uint8_t* section = spa + at;
            const size_t name = Be32(section + 12);
            if (16 + name + 8 > length) return {};
            const uint32_t plain = Be32(section + 16 + name), packed = Be32(section + 20 + name);
            if (24 + name + packed > length) return {};
            std::vector<uint8_t> xml(plain);
            z_stream stream{};
            if (inflateInit2(&stream, 16 + MAX_WBITS) != Z_OK) return {};
            stream.next_in = const_cast<uint8_t*>(section + 24 + name);
            stream.avail_in = packed;
            stream.next_out = xml.data();
            stream.avail_out = plain;
            const int result = inflate(&stream, Z_FINISH);
            const bool whole = stream.total_out == plain;
            inflateEnd(&stream);
            if (result != Z_STREAM_END || !whole) return {};
            // UTF-16, little-endian, after its byte order mark if it has one.
            const size_t skip = plain >= 2 && xml[0] == 0xFF && xml[1] == 0xFE ? 2 : 0;
            return Utf8(xml.data() + skip, plain - skip);
        }
#endif
        return {};
    }

    void LoadLocked()
    {
        if (g_strings.loaded) return;
        g_strings.loaded = true;
        const std::string xml = Xlast();
        if (xml.empty()) { LOGW("presence: the image has no XLAST to read"); return; }

        for (size_t at = 0; (at = xml.find("<LocalizedString ", at)) != std::string::npos; at++)
        {
            const size_t end = xml.find("</LocalizedString>", at);
            const size_t english = xml.find("<Translation locale=\"en-US\">", at);
            if (end == std::string::npos || english == std::string::npos || english > end) continue;
            const size_t from = xml.find('>', english) + 1;
            g_strings.text[Number(Attribute(xml, at, "id"))] = Plain(xml.substr(from, xml.find('<', from) - from));
        }
        for (size_t at = 0; (at = xml.find("<PresenceMode ", at)) != std::string::npos; at++)
            g_strings.modes[Number(Attribute(xml, at, "contextValue"))] = Number(Attribute(xml, at, "stringId"));
        for (size_t at = 0; (at = xml.find("<Context ", at)) != std::string::npos; at++)
        {
            Context& context = g_strings.contexts[Number(Attribute(xml, at, "id"))];
            context.fallback = Number(Attribute(xml, at, "defaultValue"));
            const size_t end = xml.find("</Context>", at);
            for (size_t value = xml.find("<ContextValue ", at); value < end; value = xml.find("<ContextValue ", value + 1))
                context.strings[Number(Attribute(xml, value, "value"))] = Number(Attribute(xml, value, "stringId"));
        }
        LOGI("presence: %zu modes, %zu contexts, %zu strings", g_strings.modes.size(), g_strings.contexts.size(),
             g_strings.text.size());
    }

    std::string TextOf(uint32_t id)
    {
        const auto found = g_strings.text.find(id);
        return found != g_strings.text.end() ? found->second : std::string();
    }
}

// The XLAST is read at the title's first context, on its thread, rather than
// when the guide first draws.
void presence::SetContext(uint32_t user, uint32_t id, uint32_t value)
{
    if (user >= kUsers) return;
    std::lock_guard lock(g_lock);
    LoadLocked();
    g_contexts[user][id] = value;
}

std::string presence::Text(uint32_t user)
{
    if (user >= kUsers) return {};
    std::lock_guard lock(g_lock);
    const auto& set = g_contexts[user];
    const auto mode = set.find(X_CONTEXT_PRESENCE);
    if (mode == set.end()) return {};
    LoadLocked();
    const auto string = g_strings.modes.find(mode->second);
    if (string == g_strings.modes.end()) return {};

    // "{c7}": the string of context 7's value. A property's ("{p...}") is
    // left out: the title's modes name none.
    std::string line = TextOf(string->second);
    for (size_t at = 0; (at = line.find('{', at)) != std::string::npos;)
    {
        const size_t end = line.find('}', at);
        if (end == std::string::npos) break;
        const bool context = line[at + 1] == 'c';
        const uint32_t id = Number(line.substr(at + 2, end - at - 2));
        std::string value;
        if (const auto found = g_strings.contexts.find(id); context && found != g_strings.contexts.end())
        {
            const auto setTo = set.find(id);
            const uint32_t chosen = setTo != set.end() ? setTo->second : found->second.fallback;
            if (const auto name = found->second.strings.find(chosen); name != found->second.strings.end())
                value = TextOf(name->second);
        }
        line.replace(at, end + 1 - at, value);
        at += value.size();
    }
    return line;
}
