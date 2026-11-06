// runtime/gpu/vulkan/presenter.cpp as it is, compiled into the rig: its file-static
// helpers reached from this file, its Vulkan and SDL calls renamed to presenter_stubs.cpp's,
// its steady clock and condition variables to ones that move only when the rig says, and
// what it calls of the rest of the runtime (frame_rate, stutters, the screens, the
// backdrop, gpu_time, pipeline) to the stand-ins below.
#include <algorithm>
#include <atomic>
#include <chrono>
#include <cmath>
#include <condition_variable>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <functional>
#include <mutex>
#include <shared_mutex>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#include "presenter_rig.h"

#include "console.h"
#include "crash.h"
#include "diagnostics.h"
#include "env.h"
#include "frame_rate.h"
#include "friends.h"
#include "guide.h"
#include "render_size.h"
#include "screen.h"
#include "signin.h"
#include "stutters.h"

// A condition variable whose timed wait returns at once: satisfied, or timed out with the
// rig's clock moved on by the wait's length.
namespace std
{
    struct rig_condition_variable
    {
        void notify_one() noexcept { rig::notifies++; }
        void notify_all() noexcept { rig::notifies++; }
        template <class Rep, class Period, class Predicate>
        bool wait_for(std::unique_lock<std::mutex>&, const std::chrono::duration<Rep, Period>& time, Predicate ready)
        {
            if (ready()) return true;
            rig::now += std::chrono::duration_cast<std::chrono::nanoseconds>(time).count();
            return ready();
        }
    };
}

namespace rig_frame_rate
{
    inline bool console = true;
    inline bool Console() { return console; }
}

namespace rig_stutters
{
    struct DisplayedCall { uint64_t serial; uint32_t extra; double gapMs; };
    inline std::vector<DisplayedCall> displayed;
    inline uint32_t shown = 0;
    inline uint64_t shownSerial = 0;
    inline bool On() { return true; }
    inline void Displayed(uint64_t serial, uint32_t extra, double gapMs) { displayed.push_back({ serial, extra, gapMs }); }
    inline void Shown(uint64_t serial) { shown++; shownSerial = serial; }
    inline void Mark(const char*) {}
}

namespace rig_crash
{
    inline void RegisterThread(const char*) {}
    inline void RequestExit(const char*) {}
}

// The sign-in screen's picture while the case says one is open; the others never open.
namespace rig_pictures
{
    inline bool open = false;
    inline uint32_t width = 0, height = 0;
    inline uint64_t version = 0;
    inline float outline = 0;
}
namespace rig_signin
{
    inline bool Picture(uint32_t, screen::Image& image)
    {
        if (!rig_pictures::open) return false;
        image.width = rig_pictures::width;
        image.height = rig_pictures::height;
        image.version = rig_pictures::version;
        image.outline = rig_pictures::outline;
        return true;
    }
}
namespace rig_friends
{
    inline bool Picture(uint32_t, screen::Image&) { return false; }
    inline void Ask() {}
}
namespace rig_guide
{
    inline bool Picture(uint32_t, screen::Image&) { return false; }
}

#define vk vkp
#define frame_rate rig_frame_rate
#define stutters rig_stutters
#define crash rig_crash
#define signin rig_signin
#define friends rig_friends
#define guide rig_guide
#define steady_clock rig_steady_clock
#define condition_variable rig_condition_variable
#include "../../runtime/gpu/vulkan/presenter.cpp"
#undef condition_variable
#undef steady_clock
#undef guide
#undef friends
#undef signin
#undef crash
#undef stutters
#undef frame_rate
#undef vk

// What presenter.cpp calls of the renderer, the backdrop and gpu_time.
namespace rig_calls
{
    inline bool layered = false;
    inline uint32_t records = 0, submits = 0, presentFailures = 0;
    inline vkp::backdrop::Draw draw{};
}

std::mutex& vkp::pipeline::QueueMutex()
{
    static std::mutex queue;
    return queue;
}
bool vkp::pipeline::Failed(int result, const char* where)
{
    if (result != VK_SUCCESS && std::strcmp(where, "present") == 0) rig_calls::presentFailures++;
    return result != VK_SUCCESS;
}
VkResult vkp::gpu_time::Submit(VkQueue queue, const VkSubmitInfo& submit, VkFence fence)
{
    rig_trace_submit(queue, 1, &submit, fence);
    rig_calls::submits++;
    return VK_SUCCESS;
}
bool vkp::backdrop::Create(VkDevice, VkPhysicalDevice, VkFormat) { return rig_calls::layered; }
void vkp::backdrop::Destroy() {}
void vkp::backdrop::Record(const Draw& draw)
{
    rig_calls::records++;
    rig_calls::draw = draw;
}

namespace
{
    template <class T> T Fake(uint64_t value) { return reinterpret_cast<T>(uintptr_t(value)); }
}

// What the Rust presenter reaches in the rig beyond Vulkan (src/runtime/gpu/vulkan/
// presenter.rs, mod rig) and of the C++ pipeline (src/runtime/bridge.rs): the same
// stand-ins the C++ presenter above is given.
extern "C"
{
    bool rig_picture(uint32_t* width, uint32_t* height, uint64_t* version, float* outline)
    {
        screen::Image image;
        if (!rig_signin::Picture(0, image)) return false;
        *width = image.width;
        *height = image.height;
        *version = image.version;
        *outline = image.outline;
        return true;
    }
    bool rig_backdrop_layered() { return rig_calls::layered; }
    // Whether the backdrop can be made, for both sides, from the next draw on.
    void rig_set_layered(bool layered) { rig_calls::layered = layered; }
    struct RustBackdropDraw
    {
        uint64_t commands;
        uint32_t slot;
        uint64_t target;
        uint32_t extent[2];
        uint64_t frame;
        uint32_t frameWidth, frameHeight;
        int32_t frameFrom[2], frameTo[2];
        uint64_t picture;
        int32_t pictureFrom[2], pictureTo[2];
        float outline, shown;
    };
    void rig_backdrop_record(const RustBackdropDraw* d)
    {
        vkp::backdrop::Draw draw{};
        draw.commands = Fake<VkCommandBuffer>(d->commands);
        draw.slot = d->slot;
        draw.target = Fake<VkImage>(d->target);
        draw.extent = { d->extent[0], d->extent[1] };
        draw.frame = Fake<VkImage>(d->frame);
        draw.frameWidth = d->frameWidth;
        draw.frameHeight = d->frameHeight;
        draw.frameFrom = { d->frameFrom[0], d->frameFrom[1] };
        draw.frameTo = { d->frameTo[0], d->frameTo[1] };
        draw.picture = Fake<VkImageView>(d->picture);
        draw.pictureFrom = { d->pictureFrom[0], d->pictureFrom[1] };
        draw.pictureTo = { d->pictureTo[0], d->pictureTo[1] };
        draw.outline = d->outline;
        draw.shown = d->shown;
        vkp::backdrop::Record(draw);
    }
    void rig_shown(uint64_t serial)
    {
        if (rig_stutters::On()) rig_stutters::Shown(serial);
    }

    bool mw2_vk_failed(int result, const char* where) { return vkp::pipeline::Failed(result, where); }
    void mw2_vk_queue_lock() { vkp::pipeline::QueueMutex().lock(); }
    void mw2_vk_queue_unlock() { vkp::pipeline::QueueMutex().unlock(); }

    // The Rust presenter (presenter.rs, mod rig).
    void* mw2_rig_presenter_new();
    bool mw2_rig_presenter_swapchain(void* p, uint64_t old, VkExtent2D* extent);
    void mw2_rig_wait_fill(uint32_t count, int64_t lastShownNs);
    void mw2_rig_wait_read(uint32_t* count, int64_t* lastShownNs);
    void mw2_rig_overlay_reset(void* p, uint32_t width, uint32_t height, bool layered, uint32_t pictureWidth,
                               uint32_t pictureHeight, uint64_t version, float outline);
    float mw2_rig_overlay_step(void* p, uint64_t frame, uint32_t frameWidth, uint32_t frameHeight, const int32_t from[2],
                               const int32_t to[2]);
    void mw2_rig_flow_reset(void* p, bool presentWait);
    void mw2_rig_flow_running(bool running);
    void mw2_rig_flow_show_image(uint64_t image, uint32_t width, uint32_t height, uint64_t serial);
    bool mw2_rig_flow_wait(uint64_t serial);
    void mw2_rig_flow_let_go();
    void mw2_rig_flow_draw(void* p, bool idle, bool console);
    struct FlowState;
    void mw2_rig_flow_read(void* p, FlowState* out);
}

namespace
{
    void* RustPresenter()
    {
        static void* p = mw2_rig_presenter_new();
        return p;
    }
}

extern "C"
{
    void rig_cpp_letterbox(bool stretch, uint32_t width, uint32_t height, uint32_t fw, uint32_t fh, int32_t out[6])
    {
        g.stretch = stretch;
        g.extent = { width, height };
        VkOffset3D from{}, to{};
        Letterbox(fw, fh, from, to);
        const int32_t all[6] = { from.x, from.y, from.z, to.x, to.y, to.z };
        std::memcpy(out, all, sizeof all);
    }

    // CreateSwapchain on the case's surface: whether it made one, its size, and what it
    // asked for. `asked`: the window's size (bit 0) and the formats (bit 1) were asked.
    bool rig_cpp_swapchain(const VkSurfaceCapabilitiesKHR* caps, int windowWidth, int windowHeight,
                           const VkSurfaceFormatKHR* formats, uint32_t count, uint64_t old, VkExtent2D* extent,
                           VkSwapchainCreateInfoKHR* info, uint32_t* asked)
    {
        rig::s.caps = *caps;
        rig::s.windowWidth = windowWidth;
        rig::s.windowHeight = windowHeight;
        rig::s.formats.assign(formats, formats + count);
        rig::s.windowAsked = rig::s.formatsAsked = rig::s.created = false;
        rig::s.createSwapchain = VK_SUCCESS;
        g.swapchain = Fake<VkSwapchainKHR>(old);
        g.extent = { 1280, 720 };
        const bool made = CreateSwapchain();
        *extent = g.extent;
        if (rig::s.created) *info = rig::s.createInfo;
        *asked = (rig::s.windowAsked ? 1u : 0u) | (rig::s.formatsAsked ? 2u : 0u);
        g.swapchain = VK_NULL_HANDLE;
        return made && rig::s.created;
    }

    struct Follow { uint32_t exact; double hz, period, nominal; };

    void rig_cpp_follow(bool hasMode, int numerator, int denominator, float refresh, bool presentWait, Follow* out)
    {
        rig::s.hasMode = hasMode;
        rig::s.mode = {};
        rig::s.mode.refresh_rate_numerator = numerator;
        rig::s.mode.refresh_rate_denominator = denominator;
        rig::s.mode.refresh_rate = refresh;
        g.presentWait = presentWait;
        g.nominalNs = 0;
        g.exactRate = false;
        g.running = false;
        FollowTheDisplay();
        if (g.waiter.joinable()) g.waiter.join();
        *out = { g.exactRate ? 1u : 0u, 0.0, g.periodNs, g.nominalNs };
    }

    struct DisplayState
    {
        double nominal, period;
        int64_t lastNs;
        uint64_t lastId;
        uint32_t exact;
        double buffered;
        uint64_t onTime, stretched, behind[4];
    };

    void rig_cpp_display_reset(double nominal, double period, bool exact)
    {
        g.nominalNs = nominal;
        g.periodNs = period;
        g.exactRate = exact;
        g.lastShownNs = -1;
        g.lastShownId = 0;
        g.buffered = 1.0;
        g.shownOnTime = g.stretched = 0;
        std::fill(std::begin(g.behindCount), std::end(g.behindCount), 0);
        g.guestPeriodNs = 0;
        g.guestPeriodAt = 0;
        rig_stutters::displayed.clear();
    }

    // SawDisplayed: how many Displayed calls it made, the last of them in `call`, and the
    // guest's period and time as they stand.
    uint32_t rig_cpp_display_seen(uint64_t id, int64_t at, uint64_t queued, bool console, uint64_t* serial,
                                  uint32_t* extra, double* gapMs, uint64_t* period, int64_t* periodAt)
    {
        g.lastQueued = queued;
        rig_frame_rate::console = console;
        rig_stutters::displayed.clear();
        SawDisplayed(id, at);
        if (!rig_stutters::displayed.empty())
        {
            *serial = rig_stutters::displayed.back().serial;
            *extra = rig_stutters::displayed.back().extra;
            *gapMs = rig_stutters::displayed.back().gapMs;
        }
        *period = g.guestPeriodNs.load();
        *periodAt = g.guestPeriodAt.load();
        return uint32_t(rig_stutters::displayed.size());
    }

    void rig_cpp_display_lost() { g.lastShownNs = -1; }

    void rig_cpp_display_read(DisplayState* out)
    {
        *out = { g.nominalNs, g.periodNs, g.lastShownNs, g.lastShownId, g.exactRate ? 1u : 0u, g.buffered,
                 g.shownOnTime, g.stretched, { g.behindCount[0], g.behindCount[1], g.behindCount[2], g.behindCount[3] } };
    }

    uint64_t rig_cpp_guest_blank(uint64_t period, int64_t at, int64_t now)
    {
        g.guestPeriodNs = period;
        g.guestPeriodAt = at;
        rig::now = now;
        return vkp::GuestBlankPeriod();
    }

    // ---- the overlay's fade: DrawOverlay with a picture that needs no upload

    struct OverlayStep
    {
        float fade;
        uint32_t recorded, blitted;
        float shown, outline;
        int32_t pictureFrom[2], pictureTo[2], frameFrom[2], frameTo[2];
    };

    void rig_cpp_overlay_reset(uint32_t width, uint32_t height, bool layered, uint32_t pictureWidth,
                               uint32_t pictureHeight, uint64_t version, float outline)
    {
        auto& o = g.overlay;
        g.extent = { width, height };
        g.frameIndex = 0;
        rig_calls::layered = layered;
        rig_pictures::width = pictureWidth;
        rig_pictures::height = pictureHeight;
        rig_pictures::version = version;
        rig_pictures::outline = outline;
        o.rig_signin.width = pictureWidth;
        o.rig_signin.height = pictureHeight;
        o.rig_signin.version = version;
        o.shown = &o.rig_signin;
        o.width = pictureWidth;
        o.height = pictureHeight;
        o.uploaded = version;
        o.outline = outline;
        o.everUploaded = true;
        o.layered = layered;
        o.fade = 0;
        o.frameFrom = o.frameTo = {};
        o.drawn = {};
    }

    void rig_cpp_overlay_step(int64_t now, bool picture, uint64_t frame, uint32_t frameWidth, uint32_t frameHeight,
                              const int32_t from[2], const int32_t to[2], OverlayStep* out)
    {
        rig::now = now;
        rig_pictures::open = picture;
        const uint32_t records = rig_calls::records, blits = rig::s.blits;
        Presenter::Queued shown{ Fake<VkImage>(frame), frameWidth, frameHeight, 1 };
        DrawOverlay(g.frames[0], Fake<VkImage>(0x77), shown, { from[0], from[1], 0 }, { to[0], to[1], 1 });
        const auto& d = rig_calls::draw;
        *out = { g.overlay.fade, rig_calls::records - records, rig::s.blits - blits, d.shown, d.outline,
                 { d.pictureFrom.x, d.pictureFrom.y }, { d.pictureTo.x, d.pictureTo.y },
                 { d.frameFrom.x, d.frameFrom.y }, { d.frameTo.x, d.frameTo.y } };
    }

    // ---- the frame loop and the frame queue

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

    void rig_cpp_flow_reset(bool presentWait)
    {
        g.running = true;
        g.frameQueue.clear();
        g.taken = 0;
        g.lastQueued = 0;
        g.waiting.clear();
        g.presentWait = presentWait;
        g.remakeSwapchain = false;
        g.frameIndex = 0;
        g.presented = 0;
        g.stretch = false;
        g.extent = { 1280, 720 };
        g.swapchain = Fake<VkSwapchainKHR>(0x5000);
        g.images = { Fake<VkImage>(0x5001), Fake<VkImage>(0x5002), Fake<VkImage>(0x5003) };
        g.overlay.everUploaded = false;
        g.overlay.fade = 0;
        rig_pictures::open = false;
        rig_calls::layered = false;
    }

    void rig_cpp_flow_show_image(uint64_t image, uint32_t width, uint32_t height, uint64_t serial)
    {
        vkp::ShowImage(reinterpret_cast<void*>(uintptr_t(image)), width, height, serial);
    }

    bool rig_cpp_flow_wait(uint64_t serial)
    {
        const uint32_t warnings = rig::warnings;
        vkp::WaitUntilTaken(serial);
        return rig::warnings != warnings;
    }

    void rig_cpp_flow_let_go() { LetGo(); }

    void rig_cpp_flow_running(bool running) { g.running = running; }

    void rig_cpp_flow_draw(bool idle, bool console, const FrameScript* script, FrameOut* out)
    {
        rig_frame_rate::console = console;
        rig::s.waitFences = script->waitFailed ? VK_ERROR_DEVICE_LOST : VK_SUCCESS;
        rig::s.acquire = VkResult(script->acquire);
        rig::s.imageIndex = script->imageIndex;
        rig::s.present = VkResult(script->present);
        // What a remake meets: a surface it makes a swapchain for; one whose
        // vkCreateSwapchainKHR fails, which drops the old one; a window of no size, which
        // keeps it.
        rig::s.caps = {};
        rig::s.caps.minImageCount = 2;
        rig::s.caps.currentExtent = script->remakeMade || script->remakeDrops ? VkExtent2D{ 1280, 720 } : VkExtent2D{ 0, 0 };
        rig::s.formats = { { VK_FORMAT_B8G8R8A8_UNORM, VK_COLOR_SPACE_SRGB_NONLINEAR_KHR } };
        rig::s.createSwapchain = script->remakeMade ? VK_SUCCESS : VK_ERROR_INITIALIZATION_FAILED;
        const uint32_t remakes = rig::s.remakes, begun = rig::s.begun, blits = rig::s.blits, submits = rig_calls::submits,
                       presents = rig::s.presents, failures = rig_calls::presentFailures, shown = rig_stutters::shown;
        rig::s.blitSource = 0;
        DrawOneFrame(idle);
        *out = {};
        out->remakes = rig::s.remakes - remakes;
        out->recorded = rig::s.begun - begun;
        out->shownImage = rig::s.blits != blits ? rig::s.blitSource : 0;
        out->submitted = rig_calls::submits - submits;
        out->presents = rig::s.presents - presents;
        out->hasId = out->presents && rig::s.hasId;
        out->presentId = out->hasId ? rig::s.presentId : 0;
        out->presentFailed = rig_calls::presentFailures - failures;
        out->shownCalls = rig_stutters::shown - shown;
        out->shownCallSerial = out->shownCalls ? rig_stutters::shownSerial : 0;
    }

    void rig_cpp_flow_read(FlowState* out)
    {
        *out = {};
        out->remake = g.remakeSwapchain;
        out->frameIndex = g.frameIndex;
        out->presented = g.presented.load();
        out->taken = g.taken;
        out->lastQueued = g.lastQueued.load();
        out->queued = uint32_t(g.frameQueue.size());
        for (size_t i = 0; i < g.frameQueue.size() && i < 32; i++) out->serials[i] = g.frameQueue[i].serial;
        out->waiting = uint32_t(g.waiting.size());
        for (size_t i = 0; i < g.waiting.size() && i < 32; i++) out->ids[i] = g.waiting[i];
    }
}

// The same cases on the Rust presenter, through the same stand-ins and with the same
// reading of what they recorded as the rig_cpp_ functions above.
extern "C"
{
    void rig_cpp_wait_fill(uint32_t count, int64_t lastShownNs)
    {
        g.waiting.clear();
        for (uint32_t i = 1; i <= count; i++) g.waiting.push_back(i);
        g.lastShownNs = lastShownNs;
    }
    void rig_cpp_wait_read(uint32_t* count, int64_t* lastShownNs)
    {
        *count = uint32_t(g.waiting.size());
        *lastShownNs = g.lastShownNs;
    }
    void rig_rust_wait_fill(uint32_t count, int64_t lastShownNs) { mw2_rig_wait_fill(count, lastShownNs); }
    void rig_rust_wait_read(uint32_t* count, int64_t* lastShownNs) { mw2_rig_wait_read(count, lastShownNs); }

    bool rig_rust_swapchain(const VkSurfaceCapabilitiesKHR* caps, int windowWidth, int windowHeight,
                            const VkSurfaceFormatKHR* formats, uint32_t count, uint64_t old, VkExtent2D* extent,
                            VkSwapchainCreateInfoKHR* info, uint32_t* asked)
    {
        rig::s.caps = *caps;
        rig::s.windowWidth = windowWidth;
        rig::s.windowHeight = windowHeight;
        rig::s.formats.assign(formats, formats + count);
        rig::s.windowAsked = rig::s.formatsAsked = rig::s.created = false;
        rig::s.createSwapchain = VK_SUCCESS;
        const bool made = mw2_rig_presenter_swapchain(RustPresenter(), old, extent);
        if (rig::s.created) *info = rig::s.createInfo;
        *asked = (rig::s.windowAsked ? 1u : 0u) | (rig::s.formatsAsked ? 2u : 0u);
        return made && rig::s.created;
    }

    void rig_rust_overlay_reset(uint32_t width, uint32_t height, bool layered, uint32_t pictureWidth,
                                uint32_t pictureHeight, uint64_t version, float outline)
    {
        rig_calls::layered = layered;
        rig_pictures::width = pictureWidth;
        rig_pictures::height = pictureHeight;
        rig_pictures::version = version;
        rig_pictures::outline = outline;
        mw2_rig_overlay_reset(RustPresenter(), width, height, layered, pictureWidth, pictureHeight, version, outline);
    }

    void rig_rust_overlay_step(int64_t now, bool picture, uint64_t frame, uint32_t frameWidth, uint32_t frameHeight,
                               const int32_t from[2], const int32_t to[2], OverlayStep* out)
    {
        rig::now = now;
        rig_pictures::open = picture;
        const uint32_t records = rig_calls::records, blits = rig::s.blits;
        const float fade = mw2_rig_overlay_step(RustPresenter(), frame, frameWidth, frameHeight, from, to);
        const auto& d = rig_calls::draw;
        *out = { fade, rig_calls::records - records, rig::s.blits - blits, d.shown, d.outline,
                 { d.pictureFrom.x, d.pictureFrom.y }, { d.pictureTo.x, d.pictureTo.y },
                 { d.frameFrom.x, d.frameFrom.y }, { d.frameTo.x, d.frameTo.y } };
    }

    void rig_rust_flow_reset(bool presentWait)
    {
        rig_pictures::open = false;
        rig_calls::layered = false;
        mw2_rig_flow_reset(RustPresenter(), presentWait);
    }
    void rig_rust_flow_show_image(uint64_t image, uint32_t width, uint32_t height, uint64_t serial)
    {
        mw2_rig_flow_show_image(image, width, height, serial);
    }
    bool rig_rust_flow_wait(uint64_t serial) { return mw2_rig_flow_wait(serial); }
    void rig_rust_flow_let_go() { mw2_rig_flow_let_go(); }
    void rig_rust_flow_running(bool running) { mw2_rig_flow_running(running); }

    void rig_rust_flow_read(FlowState* out) { mw2_rig_flow_read(RustPresenter(), out); }

    void rig_rust_flow_draw(bool idle, bool console, const FrameScript* script, FrameOut* out)
    {
        rig::s.waitFences = script->waitFailed ? VK_ERROR_DEVICE_LOST : VK_SUCCESS;
        rig::s.acquire = VkResult(script->acquire);
        rig::s.imageIndex = script->imageIndex;
        rig::s.present = VkResult(script->present);
        rig::s.queueSubmit = VK_SUCCESS;
        rig::s.caps = {};
        rig::s.caps.minImageCount = 2;
        rig::s.caps.currentExtent = script->remakeMade || script->remakeDrops ? VkExtent2D{ 1280, 720 } : VkExtent2D{ 0, 0 };
        rig::s.formats = { { VK_FORMAT_B8G8R8A8_UNORM, VK_COLOR_SPACE_SRGB_NONLINEAR_KHR } };
        rig::s.createSwapchain = script->remakeMade ? VK_SUCCESS : VK_ERROR_INITIALIZATION_FAILED;
        // The Rust submits through the real gpu_time::submit, off, so to vkQueueSubmit.
        const uint32_t remakes = rig::s.remakes, begun = rig::s.begun, blits = rig::s.blits, submits = rig::s.submits,
                       presents = rig::s.presents, failures = rig_calls::presentFailures, shown = rig_stutters::shown;
        rig::s.blitSource = 0;
        mw2_rig_flow_draw(RustPresenter(), idle, console);
        *out = {};
        out->remakes = rig::s.remakes - remakes;
        out->recorded = rig::s.begun - begun;
        out->shownImage = rig::s.blits != blits ? rig::s.blitSource : 0;
        out->submitted = rig::s.submits - submits;
        out->presents = rig::s.presents - presents;
        out->hasId = out->presents && rig::s.hasId;
        out->presentId = out->hasId ? rig::s.presentId : 0;
        out->presentFailed = rig_calls::presentFailures - failures;
        out->shownCalls = rig_stutters::shown - shown;
        out->shownCallSerial = out->shownCalls ? rig_stutters::shownSerial : 0;
    }
}
