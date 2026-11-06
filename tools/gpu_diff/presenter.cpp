// tools/gpu_diff/presenter.cpp: the window's, the backdrop's and gpu_time's decisions, C++
// (runtime/gpu/vulkan/presenter.cpp, backdrop.cpp and gpu_time.cpp as they are, run on
// the stand-ins of presenter_stubs.cpp) against the Rust (src/runtime/gpu/vulkan/) on
// generated cases: the decisions that are functions of values through the rig's C
// functions in src/runtime/gpu/exports.rs, and the window thread's own code (making a
// swapchain, a frame of the loop with its recording, the overlay) and the backdrop's
// draws run on the same stand-ins (presenter.rs and backdrop.rs, mod rig), every Vulkan
// call either side makes held against the other's with its arguments. No device and no
// window is made.
//
//   presenter <seed> <cases> [letterbox|swapchain|follow|display|blank|fade|push|shaders|queue|gpu_time|abi|dump=<dir>]...
//
// Exits non-zero at the first difference, printing the case.
#include <cinttypes>
#include <cstdarg>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <limits>
#include <string>
#include <vector>

#include <vulkan/vulkan.h>

namespace
{
    uint64_t g_state;
    uint32_t Next()
    {
        g_state = g_state * 6364136223846793005ull + 1442695040888963407ull;
        return uint32_t(g_state >> 33);
    }
    uint32_t Below(uint32_t n) { return n ? Next() % n : 0; }
    uint64_t Next64() { return uint64_t(Next()) << 32 | Next(); }
    bool Chance(uint32_t percent) { return Below(100) < percent; }
    float Unit() { return float(Next() >> 8) / float(1u << 24); }

    uint32_t Bits(float f) { uint32_t b; std::memcpy(&b, &f, 4); return b; }
    uint64_t Bits(double d) { uint64_t b; std::memcpy(&b, &d, 8); return b; }

    uint64_t g_compared = 0;
    [[noreturn]] void Fail(const char* part, const std::string& what)
    {
        std::printf("DIFFERENCE in %s: %s\n", part, what.c_str());
        std::fflush(stdout);
        std::exit(1);
    }
    void Same(const char* part, const char* field, uint64_t cpp, uint64_t rust, const std::string& context)
    {
        g_compared++;
        if (cpp == rust) return;
        char line[256];
        std::snprintf(line, sizeof line, "%s: C++ %" PRIu64 " (0x%" PRIx64 "), Rust %" PRIu64 " (0x%" PRIx64 ")", field, cpp,
                      cpp, rust, rust);
        Fail(part, context + " " + line);
    }
    std::string Context(const char* format, ...) __attribute__((format(printf, 1, 2)));
    std::string Context(const char* format, ...)
    {
        char text[512];
        va_list args;
        va_start(args, format);
        std::vsnprintf(text, sizeof text, format, args);
        va_end(args);
        return text;
    }
}

extern "C" size_t rig_trace_take(uint64_t* out, size_t capacity);

namespace
{
    // The calls made since rig_trace_start.
    std::vector<uint64_t> Trace()
    {
        static std::vector<uint64_t> buffer(1 << 20);
        const size_t n = rig_trace_take(buffer.data(), buffer.size());
        if (n > buffer.size()) Fail("trace", Context("%zu words, more than the rig holds", n));
        return std::vector<uint64_t>(buffer.begin(), buffer.begin() + n);
    }
    void SameCalls(const char* part, const std::vector<uint64_t>& cpp, const std::vector<uint64_t>& rust,
                   const std::string& context)
    {
        g_compared += cpp.size() + 1;
        const size_t n = std::min(cpp.size(), rust.size());
        size_t k = 0;
        while (k < n && cpp[k] == rust[k]) k++;
        if (k == n && cpp.size() == rust.size()) return;
        // The call it is in starts at the last tag (0xCA11xxx) at or before the word.
        size_t call = k < n ? k : n ? n - 1 : 0;
        while (call > 0 && (cpp.empty() || (cpp[call] >> 12) != 0xCA11)) call--;
        std::string words;
        for (size_t i = call; i < call + 24 && i < std::max(cpp.size(), rust.size()); i++)
            words += Context(" [%zu] %" PRIx64 "/%" PRIx64, i, i < cpp.size() ? cpp[i] : ~0ull, i < rust.size() ? rust[i] : ~0ull);
        Fail(part, context + Context(" calls differ at word %zu of %zu (C++) and %zu (Rust); C++/Rust from the call:", k,
                                     cpp.size(), rust.size()) + words);
    }
}

// ---- the C++ originals (presenter_cpp.cpp, backdrop_cpp.cpp, gpu_time_cpp.cpp)

struct Follow { uint32_t exact; double hz, period, nominal; };
struct DisplayState
{
    double nominal, period;
    int64_t lastNs;
    uint64_t lastId;
    uint32_t exact;
    double buffered;
    uint64_t onTime, stretched, behind[4];
};
struct OverlayStep
{
    float fade;
    uint32_t recorded, blitted;
    float shown, outline;
    int32_t pictureFrom[2], pictureTo[2], frameFrom[2], frameTo[2];
};
struct FrameScript { uint32_t remakeMade, remakeDrops, waitFailed; int32_t acquire; uint32_t imageIndex; int32_t present; };
struct FrameOut
{
    uint32_t remakes, recorded;
    uint64_t shownImage, shownSerial;
    uint32_t submitted, presents, hasId;
    uint64_t presentId;
    uint32_t presentFailed, shownCalls;
    uint64_t shownCallSerial;
};
struct FlowState
{
    uint32_t remake, frameIndex;
    uint64_t presented, taken, lastQueued;
    uint32_t queued;
    uint64_t serials[32];
    uint32_t waiting;
    uint64_t ids[32];
};
struct RigDraw
{
    uint32_t frameWidth, frameHeight;
    int32_t frame[4], picture[4];
    uint32_t extent[2];
    float outline, shown;
};
struct Recorded
{
    float push[16];
    uint32_t pushes, settles, levels;
    uint32_t level[8];
    uint32_t madeWidth, madeHeight, hasImage;
};
struct Collected
{
    uint64_t collected, nanoseconds, lastEnd;
    uint32_t anyEnd;
    uint64_t seen[64][2];
};

extern "C"
{
    void rig_cpp_letterbox(bool stretch, uint32_t width, uint32_t height, uint32_t fw, uint32_t fh, int32_t out[6]);
    bool rig_cpp_swapchain(const VkSurfaceCapabilitiesKHR* caps, int windowWidth, int windowHeight,
                           const VkSurfaceFormatKHR* formats, uint32_t count, uint64_t old, VkExtent2D* extent,
                           VkSwapchainCreateInfoKHR* info, uint32_t* asked);
    void rig_cpp_follow(bool hasMode, int numerator, int denominator, float refresh, bool presentWait, Follow* out);
    void rig_cpp_display_reset(double nominal, double period, bool exact);
    uint32_t rig_cpp_display_seen(uint64_t id, int64_t at, uint64_t queued, bool console, uint64_t* serial,
                                  uint32_t* extra, double* gapMs, uint64_t* period, int64_t* periodAt);
    void rig_cpp_display_lost();
    void rig_cpp_display_read(DisplayState* out);
    uint64_t rig_cpp_guest_blank(uint64_t period, int64_t at, int64_t now);
    void rig_cpp_overlay_reset(uint32_t width, uint32_t height, bool layered, uint32_t pictureWidth,
                               uint32_t pictureHeight, uint64_t version, float outline);
    void rig_cpp_overlay_step(int64_t now, bool picture, uint64_t frame, uint32_t frameWidth, uint32_t frameHeight,
                              const int32_t from[2], const int32_t to[2], OverlayStep* out);
    void rig_cpp_flow_reset(bool presentWait);
    void rig_cpp_flow_show_image(uint64_t image, uint32_t width, uint32_t height, uint64_t serial);
    bool rig_cpp_flow_wait(uint64_t serial);
    void rig_cpp_flow_let_go();
    void rig_cpp_flow_draw(bool idle, bool console, const FrameScript* script, FrameOut* out);
    void rig_cpp_flow_read(FlowState* out);
    void rig_cpp_flow_running(bool running);
    size_t rig_cpp_backdrop_shader(bool fragment, uint32_t* out, size_t capacity);
    void rig_cpp_backdrop_reset();
    void rig_cpp_backdrop_record(const RigDraw* d, bool hasFrame, uint32_t slot, Recorded* out);
    bool rig_cpp_gpu_start(const uint32_t* bits, uint32_t families, uint32_t family, float period, uint64_t* mask,
                           double* nanosecondsPerTick);
    int32_t rig_cpp_gpu_submit(bool accepted);
    void rig_cpp_gpu_collect(const uint64_t query[128][2], const int32_t results[64], Collected* out);
    void rig_cpp_wait_fill(uint32_t count, int64_t lastShownNs);
    void rig_cpp_wait_read(uint32_t* count, int64_t* lastShownNs);
    // The forwarders of the Rust-renderer build (presenter_forwarders.cpp).
    bool fwd_abi(char* why, size_t size);
    // The stand-ins' trace of every call (presenter_stubs.cpp).
    void rig_trace_start(uint64_t nextHandle);
    size_t rig_trace_take(uint64_t* out, size_t capacity);
}

// ---- the Rust on the stand-ins, read as the C++ is (presenter_cpp.cpp, backdrop_cpp.cpp)

extern "C"
{
    bool rig_rust_swapchain(const VkSurfaceCapabilitiesKHR* caps, int windowWidth, int windowHeight,
                            const VkSurfaceFormatKHR* formats, uint32_t count, uint64_t old, VkExtent2D* extent,
                            VkSwapchainCreateInfoKHR* info, uint32_t* asked);
    void rig_rust_wait_fill(uint32_t count, int64_t lastShownNs);
    void rig_rust_wait_read(uint32_t* count, int64_t* lastShownNs);
    void rig_rust_overlay_reset(uint32_t width, uint32_t height, bool layered, uint32_t pictureWidth,
                                uint32_t pictureHeight, uint64_t version, float outline);
    void rig_rust_overlay_step(int64_t now, bool picture, uint64_t frame, uint32_t frameWidth, uint32_t frameHeight,
                               const int32_t from[2], const int32_t to[2], OverlayStep* out);
    void rig_set_layered(bool layered);
    void rig_rust_flow_reset(bool presentWait);
    void rig_rust_flow_show_image(uint64_t image, uint32_t width, uint32_t height, uint64_t serial);
    bool rig_rust_flow_wait(uint64_t serial);
    void rig_rust_flow_let_go();
    void rig_rust_flow_draw(bool idle, bool console, const FrameScript* script, FrameOut* out);
    void rig_rust_flow_read(FlowState* out);
    void rig_rust_flow_running(bool running);
    void* rig_rust_backdrop_new();
    void rig_rust_backdrop_free(void* backdrop);
    void rig_rust_backdrop_record(void* backdrop, const RigDraw* d, bool hasFrame, uint32_t slot, Recorded* out);
}

// ---- the Rust (src/runtime/gpu/exports.rs, mod rig)

extern "C"
{
    void mw2_rig_letterbox(bool stretch, uint32_t width, uint32_t height, uint32_t fw, uint32_t fh, int32_t out[6]);
    void mw2_rig_follow(bool hasMode, int32_t numerator, int32_t denominator, float refresh, bool presentWait, Follow* out);
    void* mw2_rig_display_new(double nominal, double period, bool exact);
    void mw2_rig_display_free(void* display);
    uint32_t mw2_rig_display_seen(void* display, uint64_t id, int64_t at, uint64_t queued, bool console,
                                  void (*displayed)(void*, uint64_t, uint32_t, double), void* context, uint64_t* period,
                                  int64_t* at_out);
    void mw2_rig_display_lost(void* display);
    void mw2_rig_display_read(const void* display, DisplayState* out);
    uint64_t mw2_rig_guest_blank(uint64_t period, int64_t at, int64_t now);
    size_t mw2_rig_backdrop_shader(bool fragment, uint32_t* out, size_t capacity);
    bool mw2_rig_gpu_timing(const uint32_t* bits, uint32_t families, uint32_t family, float period, uint64_t* mask,
                            double* nanosecondsPerTick);
    void* mw2_rig_gpu_ring_new();
    void mw2_rig_gpu_ring_free(void* ring);
    int32_t mw2_rig_gpu_submit(void* ring, bool accepted);
    void mw2_rig_gpu_collect(void* ring, uint64_t mask, double nanosecondsPerTick,
                             int32_t (*read)(void*, uint32_t, uint64_t (*)[4]), void* context, Collected* out);
}

// ---- the parts

namespace
{
    // Sizes that find the edges: 0, 1, odd, even, the console's, large.
    uint32_t Side(uint32_t most)
    {
        switch (Below(6))
        {
            case 0: return Below(4);
            case 1: return 1 + Below(16);
            case 2: return 1280 + Below(3) - 1;
            case 3: return 720 + Below(3) - 1;
            default: return Below(most + 1);
        }
    }

    void Letterbox(uint32_t cases)
    {
        uint32_t n = 0;
        for (uint32_t i = 0; i < cases * 20; i++)
        {
            const bool stretch = Below(8) == 0;
            const uint32_t width = Side(8192), height = Side(8192);
            const uint32_t fw = std::max(1u, Side(4096)), fh = std::max(1u, Side(4096));
            int32_t cpp[6], rust[6];
            rig_cpp_letterbox(stretch, width, height, fw, fh, cpp);
            mw2_rig_letterbox(stretch, width, height, fw, fh, rust);
            const std::string at = Context("stretch %d window %ux%u frame %ux%u", stretch, width, height, fw, fh);
            for (int k = 0; k < 6; k++) Same("letterbox", "offset", uint32_t(cpp[k]), uint32_t(rust[k]), at);
            n++;
        }
        std::printf("letterbox: %u cases the same\n", n);
    }

    void Swapchain(uint32_t cases)
    {
        static const VkFormat kFormats[] = { VK_FORMAT_B8G8R8A8_UNORM, VK_FORMAT_B8G8R8A8_SRGB, VK_FORMAT_R8G8B8A8_UNORM,
                                             VK_FORMAT_R8G8B8A8_SRGB, VK_FORMAT_A2B10G10R10_UNORM_PACK32,
                                             VK_FORMAT_R16G16B16A16_SFLOAT };
        uint32_t n = 0, made = 0;
        for (uint32_t i = 0; i < cases * 4; i++)
        {
            VkSurfaceCapabilitiesKHR caps{};
            caps.minImageCount = Below(5);
            caps.maxImageCount = Below(9);
            caps.minImageExtent = { Side(2048), Side(2048) };
            caps.maxImageExtent = { Side(8192), Side(8192) };
            caps.currentExtent = Chance(50) ? VkExtent2D{ UINT32_MAX, Chance(70) ? UINT32_MAX : Side(4096) }
                                            : VkExtent2D{ Side(4096), Side(4096) };
            caps.currentTransform = VkSurfaceTransformFlagBitsKHR(1u << Below(9));
            const int windowWidth = int(Side(5000)) - (Chance(10) ? 6000 : 0);
            const int windowHeight = int(Side(5000)) - (Chance(10) ? 6000 : 0);
            std::vector<VkSurfaceFormatKHR> formats(Below(7));
            for (auto& f : formats) f = { kFormats[Below(6)], VkColorSpaceKHR(Chance(80) ? 0 : 1000104000 + Below(14)) };
            const uint64_t old = Chance(50) ? 0 : 0x4000 + Below(100);
            VkExtent2D extentCpp{}, extentRust{};
            VkSwapchainCreateInfoKHR infoCpp{}, infoRust{};
            uint32_t askedCpp = 0, askedRust = 0;
            // Present ids waited for and a display clock, which a swapchain made or not
            // leaves behind it.
            const uint32_t waiting = Below(4);
            const int64_t lastShown = Chance(30) ? -1 : int64_t(Below(1000000));
            rig_cpp_wait_fill(waiting, lastShown);
            rig_rust_wait_fill(waiting, lastShown);
            rig_trace_start(0x30000);
            const bool madeCpp = rig_cpp_swapchain(&caps, windowWidth, windowHeight, formats.data(),
                                                   uint32_t(formats.size()), old, &extentCpp, &infoCpp, &askedCpp);
            const std::vector<uint64_t> callsCpp = Trace();
            rig_trace_start(0x30000);
            const bool madeRust = rig_rust_swapchain(&caps, windowWidth, windowHeight, formats.data(),
                                                     uint32_t(formats.size()), old, &extentRust, &infoRust, &askedRust);
            const std::vector<uint64_t> callsRust = Trace();
            uint32_t waitingCpp = 0, waitingRust = 0;
            int64_t lastCpp = 0, lastRust = 0;
            rig_cpp_wait_read(&waitingCpp, &lastCpp);
            rig_rust_wait_read(&waitingRust, &lastRust);
            const std::string at = Context(
                "current %ux%u min %ux%u max %ux%u window %dx%d formats %zu old %" PRIx64, caps.currentExtent.width,
                caps.currentExtent.height, caps.minImageExtent.width, caps.minImageExtent.height, caps.maxImageExtent.width,
                caps.maxImageExtent.height, windowWidth, windowHeight, formats.size(), old);
            Same("swapchain", "made", madeCpp, madeRust, at);
            Same("swapchain", "width", extentCpp.width, extentRust.width, at);
            Same("swapchain", "height", extentCpp.height, extentRust.height, at);
            Same("swapchain", "asked", askedCpp, askedRust, at);
            Same("swapchain", "ids waited for", waitingCpp, waitingRust, at);
            Same("swapchain", "last present seen", uint64_t(lastCpp), uint64_t(lastRust), at);
            SameCalls("swapchain", callsCpp, callsRust, at);
            if (madeCpp)
            {
                made++;
                Same("swapchain", "sType", infoCpp.sType, infoRust.sType, at);
                Same("swapchain", "pNext", uintptr_t(infoCpp.pNext), uintptr_t(infoRust.pNext), at);
                Same("swapchain", "flags", infoCpp.flags, infoRust.flags, at);
                Same("swapchain", "surface", uintptr_t(infoCpp.surface), uintptr_t(infoRust.surface), at);
                Same("swapchain", "minImageCount", infoCpp.minImageCount, infoRust.minImageCount, at);
                Same("swapchain", "imageFormat", infoCpp.imageFormat, infoRust.imageFormat, at);
                Same("swapchain", "imageColorSpace", infoCpp.imageColorSpace, infoRust.imageColorSpace, at);
                Same("swapchain", "extent.width", infoCpp.imageExtent.width, infoRust.imageExtent.width, at);
                Same("swapchain", "extent.height", infoCpp.imageExtent.height, infoRust.imageExtent.height, at);
                Same("swapchain", "imageArrayLayers", infoCpp.imageArrayLayers, infoRust.imageArrayLayers, at);
                Same("swapchain", "imageUsage", infoCpp.imageUsage, infoRust.imageUsage, at);
                Same("swapchain", "imageSharingMode", infoCpp.imageSharingMode, infoRust.imageSharingMode, at);
                Same("swapchain", "queueFamilyIndexCount", infoCpp.queueFamilyIndexCount, infoRust.queueFamilyIndexCount, at);
                Same("swapchain", "preTransform", infoCpp.preTransform, infoRust.preTransform, at);
                Same("swapchain", "compositeAlpha", infoCpp.compositeAlpha, infoRust.compositeAlpha, at);
                Same("swapchain", "presentMode", infoCpp.presentMode, infoRust.presentMode, at);
                Same("swapchain", "clipped", infoCpp.clipped, infoRust.clipped, at);
                Same("swapchain", "oldSwapchain", uintptr_t(infoCpp.oldSwapchain), uintptr_t(infoRust.oldSwapchain), at);
            }
            n++;
        }
        std::printf("swapchain: %u cases the same, %u of them made one\n", n, made);
    }

    void FollowPart(uint32_t cases)
    {
        uint32_t n = 0;
        for (uint32_t i = 0; i < cases; i++)
        {
            const bool hasMode = Chance(90), presentWait = Chance(70);
            int numerator = 0, denominator = 0;
            float refresh = 0;
            switch (Below(5))
            {
                case 0: numerator = 60000; denominator = 1000; break;
                case 1: numerator = 60000; denominator = 1001; break;
                case 2: numerator = int(59000 + Below(2001)); denominator = Chance(80) ? 1000 : 0; break;
                case 3: numerator = int(Below(300000)); denominator = int(Below(3) * 1000); break;
                default: break;
            }
            refresh = Chance(50) ? float(Below(30000)) / 100.0f : 59.0f + Unit() * 2.0f;
            Follow cpp{}, rust{};
            rig_cpp_follow(hasMode, numerator, denominator, refresh, presentWait, &cpp);
            mw2_rig_follow(hasMode, numerator, denominator, refresh, presentWait, &rust);
            const std::string at = Context("mode %d %d/%d refresh %.9g wait %d", hasMode, numerator, denominator, refresh,
                                           presentWait);
            Same("follow", "exact", cpp.exact, rust.exact, at);
            Same("follow", "period", Bits(cpp.period), Bits(rust.period), at);
            Same("follow", "nominal", Bits(cpp.nominal), Bits(rust.nominal), at);
            n++;
        }
        std::printf("follow: %u modes the same\n", n);
    }

    struct Calls { uint32_t count; uint64_t serial; uint32_t extra; double gapMs; };
    void Displayed(void* context, uint64_t serial, uint32_t extra, double gapMs)
    {
        auto* calls = static_cast<Calls*>(context);
        calls->count++;
        calls->serial = serial;
        calls->extra = extra;
        calls->gapMs = gapMs;
    }

    void Display(uint32_t cases)
    {
        uint32_t scripts = 0, steps = 0, displayed = 0, published = 0, stretched = 0;
        for (uint32_t i = 0; i < cases / 4 + 1; i++)
        {
            const double hz = Chance(50) ? 60.0 : Chance(50) ? 60000.0 / 1001.0 : 59.0 + 2.0 * Unit();
            const double period = 1e9 / hz;
            const bool exact = Chance(50);
            const double nominal = Chance(80) ? period : 0.0;
            rig_cpp_display_reset(nominal, period, exact);
            void* rust = mw2_rig_display_new(nominal, period, exact);
            // The display's own clock, and how the frames reach it: on time, late, a
            // stall that builds latency, a burst that drains it.
            const double truePeriod = period * (1.0 + (Unit() - 0.5) * 0.02);
            int64_t at = int64_t(Next64() % (1ull << 50)) + 1000;
            uint64_t id = Below(10), queued = id;
            uint64_t periodRust = 0;
            int64_t periodAtRust = 0;
            const uint32_t length = 50 + Below(400);
            for (uint32_t step = 0; step < length; step++)
            {
                if (Chance(2))
                {
                    rig_cpp_display_lost();
                    mw2_rig_display_lost(rust);
                    continue;
                }
                const uint32_t jump = Chance(85) ? 1 : Chance(70) ? 2 + Below(3) : Below(2);
                id = Chance(2) && id > 3 ? id - Below(3) : id + jump;
                double gap = truePeriod * (Chance(80) ? 1.0 : double(1 + Below(4)));
                switch (Below(8))
                {
                    case 0: gap *= 0.9 + (Unit() - 0.5) * 1e-6; break;
                    case 1: gap *= 1.02 + (Unit() - 0.5) * 1e-6; break;
                    case 2: gap *= 0.85 + 0.3 * Unit(); break;
                    case 3: gap = double(Below(100000000)); break;
                    default: gap *= 1.0 + (Unit() - 0.5) * 0.01; break;
                }
                at += int64_t(gap);
                if (Chance(5)) queued = id + Below(6);
                else if (Chance(20)) queued = queued + Below(3);
                else if (Chance(10)) queued = id > 2 ? id - Below(3) : 0;
                const bool console = Chance(90);
                uint64_t serial = 0, periodCpp = 0;
                uint32_t extra = 0;
                double gapMs = 0;
                int64_t periodAtCpp = 0;
                const uint32_t countCpp =
                    rig_cpp_display_seen(id, at, queued, console, &serial, &extra, &gapMs, &periodCpp, &periodAtCpp);
                Calls calls{};
                uint64_t guest = 0;
                int64_t guestAt = 0;
                if (mw2_rig_display_seen(rust, id, at, queued, console, Displayed, &calls, &guest, &guestAt))
                {
                    periodRust = guest;
                    periodAtRust = guestAt;
                    published++;
                }
                const std::string context =
                    Context("script %u step %u id %" PRIu64 " at %" PRId64 " queued %" PRIu64 " console %d", i, step, id,
                            at, queued, console);
                Same("display", "Displayed calls", countCpp, calls.count, context);
                if (countCpp)
                {
                    displayed++;
                    Same("display", "Displayed serial", serial, calls.serial, context);
                    Same("display", "Displayed blanks", extra, calls.extra, context);
                    Same("display", "Displayed gap", Bits(gapMs), Bits(calls.gapMs), context);
                }
                Same("display", "guestPeriodNs", periodCpp, periodRust, context);
                Same("display", "guestPeriodAt", uint64_t(periodAtCpp), uint64_t(periodAtRust), context);
                DisplayState cpp{}, r{};
                rig_cpp_display_read(&cpp);
                mw2_rig_display_read(rust, &r);
                Same("display", "period", Bits(cpp.period), Bits(r.period), context);
                Same("display", "nominal", Bits(cpp.nominal), Bits(r.nominal), context);
                Same("display", "lastShownNs", uint64_t(cpp.lastNs), uint64_t(r.lastNs), context);
                Same("display", "lastShownId", cpp.lastId, r.lastId, context);
                Same("display", "buffered", Bits(cpp.buffered), Bits(r.buffered), context);
                Same("display", "shownOnTime", cpp.onTime, r.onTime, context);
                Same("display", "stretched", cpp.stretched, r.stretched, context);
                for (int k = 0; k < 4; k++) Same("display", "behindCount", cpp.behind[k], r.behind[k], context);
                stretched += cpp.stretched != 0 && step == length - 1;
                steps++;
            }
            mw2_rig_display_free(rust);
            scripts++;
        }
        std::printf("display: %u scripts of %u steps the same, %u Displayed calls, %u periods published, %u scripts "
                    "ended stretched\n",
                    scripts, steps, displayed, published, stretched);
    }

    void Blank(uint32_t cases)
    {
        uint32_t n = 0;
        for (uint32_t i = 0; i < cases * 4; i++)
        {
            const uint64_t period = Chance(10) ? 0 : 16000000 + Below(1000000);
            const int64_t at = int64_t(Next64() >> 4);
            const int64_t now = at + int64_t(Chance(50) ? 500000000 + int(Below(3)) - 1 : Below(1000000000)) -
                                (Chance(5) ? 2000000000 : 0);
            const uint64_t cpp = rig_cpp_guest_blank(period, at, now);
            const uint64_t rust = mw2_rig_guest_blank(period, at, now);
            Same("blank", "GuestBlankPeriod", cpp, rust, Context("period %" PRIu64 " at %" PRId64 " now %" PRId64, period, at, now));
            n++;
        }
        std::printf("blank: %u cases the same\n", n);
    }

    void Fade(uint32_t cases)
    {
        uint32_t scripts = 0, steps = 0, records = 0;
        for (uint32_t i = 0; i < cases / 8 + 1; i++)
        {
            const uint32_t width = 320 + Below(3000), height = 240 + Below(2000);
            const bool layered = Chance(70);
            const uint32_t pw = Chance(10) ? width + 1 + Below(10) : 200 + Below(width - 100);
            const uint32_t ph = Chance(10) ? height + 1 + Below(10) : 150 + Below(height - 100);
            const uint64_t version = 1 + Below(100);
            const float outline = float(Below(12)) * 0.5f + Unit();
            rig_cpp_overlay_reset(width, height, layered, pw, ph, version, outline);
            rig_rust_overlay_reset(width, height, layered, pw, ph, version, outline);
            int64_t now = int64_t(Next64() >> 8) + 1;
            bool picture = Chance(70);
            // The backdrop lost, or found, while the screen is shut: the fade out goes on
            // only as it was laid out. (Open again, it is laid out as it was: a new
            // layout uploads the picture, which the rig has no buffer for.)
            const bool flipWhenShut = Chance(30);
            for (uint32_t step = 0, length = 20 + Below(200); step < length; step++)
            {
                if (Chance(8)) picture = !picture;
                switch (Below(6))
                {
                    case 0: now += 0; break;
                    case 1: now += int64_t(Below(1000)); break;
                    case 2: now += 100000000 + int64_t(Below(900000000)); break;
                    default: now += 1000000 + int64_t(Below(40000000)); break;
                }
                rig_set_layered(flipWhenShut && !picture ? !layered : layered);
                const bool frame = Chance(70);
                const uint32_t fw = 1 + Below(1280), fh = 1 + Below(720);
                const int32_t from[2] = { int32_t(Below(200)), int32_t(Below(200)) };
                const int32_t to[2] = { from[0] + int32_t(Below(width)), from[1] + int32_t(Below(height)) };
                OverlayStep cpp{}, rust{};
                rig_trace_start(0x40000);
                rig_cpp_overlay_step(now, picture, frame ? 0x8000 + step : 0, fw, fh, from, to, &cpp);
                const std::vector<uint64_t> callsCpp = Trace();
                rig_trace_start(0x40000);
                rig_rust_overlay_step(now, picture, frame ? 0x8000 + step : 0, fw, fh, from, to, &rust);
                const std::vector<uint64_t> callsRust = Trace();
                const std::string context = Context("script %u step %u layered %d picture %d frame %d at %" PRId64, i, step,
                                                    layered, picture, frame, now);
                Same("fade", "fade", Bits(cpp.fade), Bits(rust.fade), context);
                Same("fade", "backdrops drawn", cpp.recorded, rust.recorded, context);
                Same("fade", "blits", cpp.blitted, rust.blitted, context);
                SameCalls("fade", callsCpp, callsRust, context);
                if (cpp.recorded)
                {
                    records++;
                    Same("fade", "shown", Bits(cpp.shown), Bits(rust.shown), context);
                    Same("fade", "outline", Bits(cpp.outline), Bits(rust.outline), context);
                    Same("fade", "pictureFrom.x", uint32_t(cpp.pictureFrom[0]), uint32_t(rust.pictureFrom[0]), context);
                    Same("fade", "pictureFrom.y", uint32_t(cpp.pictureFrom[1]), uint32_t(rust.pictureFrom[1]), context);
                    Same("fade", "pictureTo.x", uint32_t(cpp.pictureTo[0]), uint32_t(rust.pictureTo[0]), context);
                    Same("fade", "pictureTo.y", uint32_t(cpp.pictureTo[1]), uint32_t(rust.pictureTo[1]), context);
                    Same("fade", "frameFrom.x", uint32_t(cpp.frameFrom[0]), uint32_t(rust.frameFrom[0]), context);
                    Same("fade", "frameFrom.y", uint32_t(cpp.frameFrom[1]), uint32_t(rust.frameFrom[1]), context);
                    Same("fade", "frameTo.x", uint32_t(cpp.frameTo[0]), uint32_t(rust.frameTo[0]), context);
                    Same("fade", "frameTo.y", uint32_t(cpp.frameTo[1]), uint32_t(rust.frameTo[1]), context);
                }
                else if (frame && layered && picture && pw <= width && ph <= height)
                    Fail("fade", context + " the C++ drew no backdrop");
                steps++;
            }
            scripts++;
        }
        std::printf("fade: %u scripts of %u frames the same, %u backdrops drawn\n", scripts, steps, records);
    }

    void Push(uint32_t cases)
    {
        uint32_t scripts = 0, draws = 0, remakes = 0;
        for (uint32_t i = 0; i < cases / 8 + 1; i++)
        {
            rig_cpp_backdrop_reset();
            void* rust = rig_rust_backdrop_new();
            uint32_t madeW = 0, madeH = 0;
            uint32_t fw = 1 + Below(1280), fh = 1 + Below(720);
            for (uint32_t step = 0, length = 5 + Below(40); step < length; step++)
            {
                if (Chance(15))
                {
                    switch (Below(4))
                    {
                        case 0: fw = fh = 0; break;
                        case 1: fw = 16; fh = 16; break;
                        case 2: fw = Below(3); fh = Below(3); break;
                        default: fw = 1 + Below(2560); fh = 1 + Below(1440); break;
                    }
                }
                const bool hasFrame = Chance(75);
                RigDraw d{};
                d.frameWidth = fw;
                d.frameHeight = fh;
                d.extent[0] = Side(4096);
                d.extent[1] = Side(4096);
                for (int k = 0; k < 4; k++) d.frame[k] = int32_t(Below(3000)) - (Chance(5) ? 3000 : 0);
                if (Chance(30)) { d.frame[2] = d.frame[0] + int32_t(Below(2000)); d.frame[3] = d.frame[1] + int32_t(Below(2000)); }
                for (int k = 0; k < 4; k++) d.picture[k] = int32_t(Below(3000)) - (Chance(5) ? 3000 : 0);
                d.outline = Chance(20) ? 0.0f : Unit() * 8.0f;
                d.shown = Chance(10) ? 0.0f : Chance(10) ? 1.0f : Unit();
                const uint32_t slot = Below(5);
                const uint64_t handles = 0x1000000 + uint64_t(draws) * 64;
                Recorded cpp{}, r{};
                rig_trace_start(handles);
                rig_cpp_backdrop_record(&d, hasFrame, slot, &cpp);
                const std::vector<uint64_t> callsCpp = Trace();
                rig_trace_start(handles);
                rig_rust_backdrop_record(rust, &d, hasFrame, slot, &r);
                const std::vector<uint64_t> callsRust = Trace();
                const std::string context =
                    Context("script %u step %u frame %d %ux%u slot %u made %ux%u", i, step, hasFrame, fw, fh, slot, madeW, madeH);
                Same("push", "settles", cpp.settles, r.settles, context);
                Same("push", "blur levels", cpp.levels, r.levels, context);
                for (int k = 0; k < 8; k++) Same("push", "blur level size", cpp.level[k], r.level[k], context);
                Same("push", "blur made for width", cpp.madeWidth, r.madeWidth, context);
                Same("push", "blur made for height", cpp.madeHeight, r.madeHeight, context);
                Same("push", "blur image", cpp.hasImage, r.hasImage, context);
                Same("push", "pushes", cpp.pushes, r.pushes, context);
                for (int k = 0; k < 16; k++) Same("push", "push constant", Bits(cpp.push[k]), Bits(r.push[k]), context + Context(" [%d]", k));
                SameCalls("push", callsCpp, callsRust, context);
                if (cpp.madeWidth != madeW || cpp.madeHeight != madeH) remakes++;
                madeW = cpp.madeWidth;
                madeH = cpp.madeHeight;
                draws++;
            }
            rig_rust_backdrop_free(rust);
            scripts++;
        }
        std::printf("push: %u scripts of %u draws the same, %u blurs made again\n", scripts, draws, remakes);
    }

    void Shaders()
    {
        for (bool fragment : { false, true })
        {
            std::vector<uint32_t> cpp(1 << 16), rust(1 << 16);
            const size_t a = rig_cpp_backdrop_shader(fragment, cpp.data(), cpp.size());
            const size_t b = mw2_rig_backdrop_shader(fragment, rust.data(), rust.size());
            Same("shaders", "words", a, b, fragment ? "pixel" : "vertex");
            for (size_t k = 0; k < a; k++) Same("shaders", "word", cpp[k], rust[k], Context("%s word %zu", fragment ? "pixel" : "vertex", k));
            std::printf("shaders: the %s shader's %zu words the same\n", fragment ? "pixel" : "vertex", a);
        }
    }

    int32_t Result(bool present)
    {
        const uint32_t r = Below(100);
        if (r < 70) return VK_SUCCESS;
        if (r < 80) return VK_SUBOPTIMAL_KHR;
        if (r < 90) return VK_ERROR_OUT_OF_DATE_KHR;
        if (r < 95) return VK_ERROR_DEVICE_LOST;
        return present ? VK_ERROR_SURFACE_LOST_KHR : (Chance(50) ? VK_TIMEOUT : VK_NOT_READY);
    }

    void Queue(uint32_t cases)
    {
        uint32_t scripts = 0, steps = 0, shown = 0, logged = 0, letGo = 0;
        for (uint32_t i = 0; i < cases / 8 + 1; i++)
        {
            const bool presentWait = Chance(70);
            rig_cpp_flow_reset(presentWait);
            rig_rust_flow_reset(presentWait);
            uint64_t serial = 0;
            for (uint32_t step = 0, length = 20 + Below(300); step < length; step++)
            {
                std::string context = Context("script %u step %u", i, step);
                const uint32_t what = Below(100);
                if (what < 35)
                {
                    serial += Chance(95) ? 1 : 2;
                    const uint32_t w = 1 + Below(1280), h = 1 + Below(720);
                    rig_cpp_flow_show_image(0x100000 + serial, w, h, serial);
                    rig_rust_flow_show_image(0x100000 + serial, w, h, serial);
                    context += Context(" show %" PRIu64, serial);
                }
                else if (what < 50)
                {
                    const uint64_t wanted = Chance(10) ? 0 : serial > 4 ? serial - Below(5) : serial;
                    const bool cpp = rig_cpp_flow_wait(wanted);
                    const bool r = rig_rust_flow_wait(wanted);
                    context += Context(" wait %" PRIu64, wanted);
                    Same("queue", "logged", cpp, r, context);
                    logged += cpp;
                }
                else if (what < 53)
                {
                    rig_cpp_flow_let_go();
                    rig_rust_flow_let_go();
                    context += " let go";
                    letGo++;
                }
                else if (what < 54)
                {
                    const bool running = Chance(30);
                    rig_cpp_flow_running(running);
                    rig_rust_flow_running(running);
                    context += Context(" running %d", running);
                }
                else
                {
                    FrameScript script{};
                    script.remakeMade = Chance(70);
                    script.remakeDrops = !script.remakeMade && Chance(40);
                    script.waitFailed = Chance(3);
                    script.acquire = Result(false);
                    script.imageIndex = Below(3);
                    script.present = Result(true);
                    const bool idle = Chance(10), console = Chance(80);
                    FrameOut cpp{}, r{};
                    const uint64_t handles = 0x2000000 + uint64_t(steps) * 64;
                    rig_trace_start(handles);
                    rig_cpp_flow_draw(idle, console, &script, &cpp);
                    const std::vector<uint64_t> callsCpp = Trace();
                    rig_trace_start(handles);
                    rig_rust_flow_draw(idle, console, &script, &r);
                    const std::vector<uint64_t> callsRust = Trace();
                    context += Context(" draw idle %d console %d remake %u/%u wait %u acquire %d present %d", idle, console,
                                       script.remakeMade, script.remakeDrops, script.waitFailed, script.acquire, script.present);
                    Same("queue", "remakes", cpp.remakes, r.remakes, context);
                    Same("queue", "recorded", cpp.recorded, r.recorded, context);
                    Same("queue", "frame shown", cpp.shownImage, r.shownImage, context);
                    Same("queue", "submitted", cpp.submitted, r.submitted, context);
                    Same("queue", "presents", cpp.presents, r.presents, context);
                    Same("queue", "present id given", cpp.hasId, r.hasId, context);
                    Same("queue", "present id", cpp.presentId, r.presentId, context);
                    Same("queue", "present failures", cpp.presentFailed, r.presentFailed, context);
                    Same("queue", "Shown calls", cpp.shownCalls, r.shownCalls, context);
                    Same("queue", "Shown serial", cpp.shownCallSerial, r.shownCallSerial, context);
                    SameCalls("queue", callsCpp, callsRust, context);
                    shown += cpp.shownImage != 0;
                }
                FlowState cpp{}, r{};
                rig_cpp_flow_read(&cpp);
                rig_rust_flow_read(&r);
                Same("queue", "remake", cpp.remake, r.remake, context);
                Same("queue", "frameIndex", cpp.frameIndex, r.frameIndex, context);
                Same("queue", "presented", cpp.presented, r.presented, context);
                Same("queue", "taken", cpp.taken, r.taken, context);
                Same("queue", "lastQueued", cpp.lastQueued, r.lastQueued, context);
                Same("queue", "queued", cpp.queued, r.queued, context);
                for (uint32_t k = 0; k < cpp.queued && k < 32; k++) Same("queue", "queued serial", cpp.serials[k], r.serials[k], context);
                Same("queue", "waiting", cpp.waiting, r.waiting, context);
                for (uint32_t k = 0; k < cpp.waiting && k < 32; k++) Same("queue", "present id waited for", cpp.ids[k], r.ids[k], context);
                steps++;
            }
            scripts++;
        }
        // The Rust presenter's shared state is the forwarders' too (abi): as before a start.
        rig_rust_flow_reset(false);
        rig_rust_flow_running(false);
        std::printf("queue: %u scripts of %u steps the same, %u frames shown, %u waits gave up with a line, %u let-gos\n",
                    scripts, steps, shown, logged, letGo);
    }

    struct Queries
    {
        uint64_t query[128][2];
        int32_t results[64];
    };
    int32_t Read(void* context, uint32_t pair, uint64_t (*out)[4])
    {
        const auto* q = static_cast<const Queries*>(context);
        (*out)[0] = q->query[2 * pair][0];
        (*out)[1] = q->query[2 * pair][1];
        (*out)[2] = q->query[2 * pair + 1][0];
        (*out)[3] = q->query[2 * pair + 1][1];
        return q->results[pair];
    }

    void GpuTime(uint32_t cases)
    {
        uint32_t scripts = 0, submits = 0, collects = 0, busy = 0, off = 0;
        for (uint32_t i = 0; i < cases / 8 + 1; i++)
        {
            static const uint32_t kBits[] = { 36, 48, 64 };
            std::vector<uint32_t> bits(1 + Below(4));
            for (auto& b : bits) b = Chance(5) ? 0 : kBits[Below(3)];
            const uint32_t family = Chance(5) ? uint32_t(bits.size()) + Below(2) : Below(uint32_t(bits.size()));
            const float period = Chance(5) ? -Unit() : Chance(5) ? 0.0f : Chance(50) ? 1.0f : 0.1f + Unit() * 80.0f;
            uint64_t maskCpp = 0, maskRust = 0;
            double nsCpp = 0, nsRust = 0;
            const bool onCpp = rig_cpp_gpu_start(bits.data(), uint32_t(bits.size()), family, period, &maskCpp, &nsCpp);
            const bool onRust = mw2_rig_gpu_timing(bits.data(), uint32_t(bits.size()), family, period, &maskRust, &nsRust);
            const std::string start = Context("script %u family %u of %zu bits %u period %.9g", i, family, bits.size(),
                                              family < bits.size() ? bits[family] : 0, period);
            Same("gpu_time", "on", onCpp, onRust, start);
            scripts++;
            if (!onCpp) { off++; continue; }
            Same("gpu_time", "mask", maskCpp, maskRust, start);
            Same("gpu_time", "nanoseconds per tick", Bits(nsCpp), Bits(nsRust), start);
            const uint64_t mask = maskCpp;
            void* ring = mw2_rig_gpu_ring_new();
            Queries q{};
            // The GPU's clock, near the top of the counter so it wraps, with the bits above
            // the mask set as a driver may leave them.
            uint64_t clock = (mask - Below(1u << 20) * uint64_t(1 + Below(1000))) & mask;
            std::vector<int32_t> pairOf;
            for (uint32_t step = 0, length = 50 + Below(500); step < length; step++)
            {
                const std::string context = start + Context(" step %u", step);
                if (Chance(55))
                {
                    const bool accepted = Chance(92);
                    const int32_t cpp = rig_cpp_gpu_submit(accepted);
                    const int32_t rust = mw2_rig_gpu_submit(ring, accepted);
                    Same("gpu_time", "pair", uint32_t(cpp), uint32_t(rust), context + Context(" accepted %d", accepted));
                    if (cpp >= 0)
                    {
                        // Its timestamps, written or not yet.
                        const uint32_t pair = uint32_t(cpp);
                        const uint64_t start = clock + (Chance(20) ? 0 : Below(50000));
                        const uint64_t end = start + (Chance(5) ? 0 : 1 + Below(20000000));
                        clock = Chance(30) ? start + Below(1000) : end;
                        const uint64_t high = mask == ~0ull || Chance(50) ? 0 : (Next64() & ~mask);
                        q.query[2 * pair][0] = (start & mask) | high;
                        q.query[2 * pair + 1][0] = (end & mask) | high;
                        q.query[2 * pair][1] = Chance(90);
                        q.query[2 * pair + 1][1] = Chance(85);
                        q.results[pair] = Chance(90) ? VK_SUCCESS : Chance(70) ? VK_NOT_READY : VK_ERROR_DEVICE_LOST;
                    }
                    submits++;
                }
                else
                {
                    // Some of what was not there yet arrives.
                    for (uint32_t pair = 0; pair < 64; pair++)
                    {
                        if (Chance(50)) q.query[2 * pair][1] = q.query[2 * pair + 1][1] = 1;
                        if (Chance(50)) q.results[pair] = VK_SUCCESS;
                    }
                    Collected cpp{}, rust{};
                    rig_cpp_gpu_collect(q.query, q.results, &cpp);
                    mw2_rig_gpu_collect(ring, mask, nsCpp, Read, &q, &rust);
                    Same("gpu_time", "collected", cpp.collected, rust.collected, context);
                    Same("gpu_time", "busy nanoseconds", cpp.nanoseconds, rust.nanoseconds, context);
                    Same("gpu_time", "lastEnd", cpp.lastEnd, rust.lastEnd, context);
                    Same("gpu_time", "anyEnd", cpp.anyEnd, rust.anyEnd, context);
                    for (int k = 0; k < 64; k++)
                    {
                        Same("gpu_time", "seen start", cpp.seen[k][0], rust.seen[k][0], context);
                        Same("gpu_time", "seen end", cpp.seen[k][1], rust.seen[k][1], context);
                    }
                    busy += cpp.nanoseconds != 0;
                    collects++;
                }
            }
            mw2_rig_gpu_ring_free(ring);
        }
        std::printf("gpu_time: %u scripts (%u off) of %u submissions and %u collections the same, %u collections busy\n",
                    scripts, off, submits, collects, busy);
    }

    // dump=<dir>: the backdrop's shaders as the Rust writes them, for spirv-val; the
    // shaders part has found the C++'s the same.
    void Dump(const std::string& dir)
    {
        for (bool fragment : { false, true })
        {
            std::vector<uint32_t> words(1 << 16);
            words.resize(mw2_rig_backdrop_shader(fragment, words.data(), words.size()));
            const std::string path = dir + (fragment ? "/builtin_backdrop_pixel.spv" : "/builtin_backdrop_vertex.spv");
            std::FILE* f = std::fopen(path.c_str(), "wb");
            if (!f) Fail("dump", "cannot write " + path);
            std::fwrite(words.data(), 4, words.size(), f);
            std::fclose(f);
            std::printf("dump: %s, %zu words\n", path.c_str(), words.size());
        }
    }

    void Abi()
    {
        char why[256] = "";
        if (!fwd_abi(why, sizeof why)) Fail("abi", why);
        std::printf("abi: the forwarders reach the Rust with their arguments whole\n");
    }
}

int main(int argc, char** argv)
{
    if (argc < 3)
    {
        std::fprintf(stderr, "usage: presenter <seed> <cases> [letterbox|swapchain|follow|display|blank|fade|push|shaders|"
                             "queue|gpu_time|abi]...\n");
        return 2;
    }
    g_state = std::strtoull(argv[1], nullptr, 10) * 0x2545F4914F6CDD1Dull + 1;
    const uint32_t cases = uint32_t(std::strtoul(argv[2], nullptr, 10));
    for (int i = 3; i < argc; i++)
    {
        const std::string what = argv[i];
        if (what == "letterbox") Letterbox(cases);
        else if (what == "swapchain") Swapchain(cases);
        else if (what == "follow") FollowPart(cases);
        else if (what == "display") Display(cases);
        else if (what == "blank") Blank(cases);
        else if (what == "fade") Fade(cases);
        else if (what == "push") Push(cases);
        else if (what == "shaders") Shaders();
        else if (what == "queue") Queue(cases);
        else if (what == "gpu_time") GpuTime(cases);
        else if (what == "abi") Abi();
        else if (what.rfind("dump=", 0) == 0) Dump(what.substr(5));
        else { std::fprintf(stderr, "unknown part %s\n", argv[i]); return 2; }
    }
    std::printf("%" PRIu64 " fields compared\n", g_compared);
    return 0;
}
