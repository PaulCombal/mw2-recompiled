#include "runtime/kernel/guest_format.h"

#ifdef NDEBUG
#undef NDEBUG
#endif

#include <array>
#include <cassert>
#include <cstdint>
#include <vector>

struct Args
{
    std::vector<uint64_t> v; size_t i = 0;
    uint64_t next() { return i < v.size() ? v[i++] : 0; }
};

static std::array<uint8_t, 1024> mem{};

static uint32_t putA(uint32_t at, const char* s)
{
    for (; *s; s++) mem[at++] = uint8_t(*s);
    mem[at] = 0;
    return at;
}
static void putW(uint32_t at, const char16_t* s)
{
    for (;; s++, at += 2) { reinterpret_cast<be16*>(&mem[at])[0] = uint16_t(*s); if (!*s) break; }
}

static std::string run(const char* fmt, std::vector<uint64_t> a, bool* unsup = nullptr)
{
    putA(0x100, fmt);
    Args args{std::move(a)};
    auto r = kernel::fmt::Format(mem.data(), 0x100, false, args);
    if (unsup) *unsup = r.unsupported;
    std::string s; for (char32_t c : r.text) s += char(c);
    return s;
}

int main()
{
    putA(0x200, "abc");
    putW(0x300, u"wide");

    assert(run("\\Device\\cache%d", {3}) == "\\Device\\cache3");
    assert(run("\\Device\\Harddisk0\\Cache%u\\", {1}) == "\\Device\\Harddisk0\\Cache1\\");
    assert(run("%d", {uint64_t(uint32_t(-5))}) == "-5");
    assert(run("%u", {uint64_t(uint32_t(-1))}) == "4294967295");
    assert(run("%08x ", {0xBEEF}) == "0000beef ");
    assert(run("%s 0x%08X", {0x200, 0xCAFE}) == "abc 0x0000CAFE");
    assert(run("%25s:", {0x200}) == "                      abc:");
    assert(run("%-5s|", {0x200}) == "abc  |");
    assert(run("%.2s", {0x200}) == "ab");
    assert(run("%s 0x%02x: ", {0x200, 7}) == "abc 0x07: ");
    assert(run("%d%s", {12, 0x200}) == "12abc");
    assert(run("%5d|%-5d|%+d", {42, 42, 42}) == "   42|42   |+42");
    assert(run("%*d", {4, 7}) == "   7");
    assert(run("%s", {0}) == "(null)");
    assert(run("100%%", {}) == "100%");
    assert(run("%I64d", {uint64_t(-1234567890123ll)}) == "-1234567890123");
    assert(run("%hd", {0x12345}) == "9029");
    assert(run("%ls", {0x300}) == "wide");
    assert(run("%S", {0x300}) == "wide");
    assert(run("%#x", {255}) == "0xff");
    assert(run("%c", {'Z'}) == "Z");
    bool u = false;
    run("%f", {0}, &u);
    assert(u);

    // Wide format: %s is wide, %S narrow.
    putW(0x100, u"%s/%S");
    Args a2{{0x300, 0x200}};
    auto r = kernel::fmt::Format(mem.data(), 0x100, true, a2);
    assert(r.text == U"wide/abc");
    return 0;
}
