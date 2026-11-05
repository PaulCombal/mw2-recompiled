// The audio render driver: the 5.333 ms mixer cadence and the host output.
//
// Pacing: on the console the audio hardware's clock calls the driver, a frame
// for every 256 samples it plays. Here the playback device's clock does: each
// time the device takes sound from SDL's queue, SDL tells the worker (a get
// callback) how much it took and how much is left, and the worker calls the
// title's callback until the queue again holds a take and three frames more
// (kLeadFrames). Without a device the wall clock calls it at the nominal rate, because
// the title's sound state machines advance inside it and a silent run must
// still finish its sounds. (src/runtime/kernel/audio.rs does the same.)
#include <ppc_recomp_shared.h>
#include "audio.h"
#include "../diagnostics.h"
#include "../env.h"
#include "../guest.h"
#include "../log.h"
#include "../crash.h"
#include "../kernel/kernel.h"

#ifdef MW2_USE_SDL
#include <SDL3/SDL.h>
#endif

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cmath>
#include <condition_variable>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <mutex>
#include <thread>

namespace
{
    using Clock = std::chrono::steady_clock;

    constexpr uint32_t kFrequency = 48000;
    constexpr uint32_t kChannels = 6;
    constexpr uint32_t kSamplesPerChannel = 256;
    constexpr uint32_t kFrameFloats = kChannels * kSamplesPerChannel;
    constexpr uint32_t kFrameBytes = kFrameFloats * sizeof(float);
    constexpr auto     kFrameInterval = std::chrono::microseconds(5333);

    // Beyond a take and the lead, the host queue drops frames past this many
    // (128 ms of frames of 5.333 ms): a guard for a device that stalls with its
    // queue full.
    constexpr uint32_t kHighWaterFrames = 24;

    constexpr uint32_t kMaxClients = 8;
    constexpr uint32_t kHandleTag = 0x41550000u;
    // Twice what a guest thread gets: the mixer's frames are deep, and an
    // overflow here lands in whichever allocation sits below the stack.
    constexpr uint32_t kStackSize = 1024u * 1024u;

    struct Client
    {
        bool inUse = false;
        uint32_t callback = 0;
        uint32_t context = 0;
        Clock::time_point next{};
    };

    std::mutex g_lock;
    std::condition_variable g_wake;
    Client g_clients[kMaxClients];
    std::thread g_worker;
    bool g_workerStarted = false;

    // The audio thread's, reported in a player's build too: underruns are what
    // a crackle in it would be.
    std::atomic<uint64_t> g_callbacks{ 0 }, g_frames{ 0 }, g_dropped{ 0 }, g_underruns{ 0 };
    // Clients registered: a take with none has nothing to wait for, as at the
    // run's end.
    std::atomic<uint32_t> g_registered{ 0 };
    std::atomic<bool> g_outputOpen{ false };

    // The device's takes, from SDL's thread to the worker: what the queue held
    // after the last one and its size, and how much the takes since the worker
    // last looked asked for. SDL calls with its stream locked, so this lock is
    // taken after SDL's and the worker holds it for nothing else.
    std::mutex g_takesLock;
    std::condition_variable g_taken;
    bool g_takePending = false;
    size_t g_takeLeft = 0, g_takeLast = 0, g_takeAsked = 0;
    std::atomic<size_t> g_lastTake{ 0 };   // for SubmitFrame's limit

    // What the queue keeps beyond a take for the worker to refill in time, in
    // frames: how late the worker and the title's mixer thread may be
    // scheduled (the measurement is in src/runtime/kernel/audio.rs).
    constexpr uint32_t kLeadFrames = 3;

#ifdef MW2_USE_SDL
    SDL_AudioStream* g_stream = nullptr;
    std::mutex g_streamLock;
#endif

    // MW2_NO_AUDIO=1: no playback device; the mixer and the decoder still run.
    bool OutputDisabled()
    {
        static const bool off = env::Flag("MW2_NO_AUDIO");
        return off;
    }

#ifdef MW2_USE_SDL
    // SDL's device thread, before each take, with the stream locked (SDL's
    // mutexes are reentrant): `asked` bytes from a queue holding `queued`, and
    // `shortBy` of them not there, which become silence.
    void SDLCALL Taking(void*, SDL_AudioStream* stream, int shortBy, int asked)
    {
        const size_t queued = size_t(std::max(SDL_GetAudioStreamQueued(stream), 0));
        const size_t want = size_t(std::max(asked, 0));
        if (shortBy > 0 && g_frames.load(std::memory_order_relaxed) && g_registered.load(std::memory_order_relaxed))
            g_underruns++;
        {
            std::lock_guard<std::mutex> guard(g_takesLock);
            g_takePending = true;
            g_takeLeft = queued > want ? queued - want : 0;
            g_takeLast = want;
            g_takeAsked += want;
        }
        g_lastTake.store(want, std::memory_order_relaxed);
        g_taken.notify_one();
    }
#endif

    void OpenOutput()
    {
#ifdef MW2_USE_SDL
        if (OutputDisabled()) { LOGI("audio: output disabled (MW2_NO_AUDIO)"); return; }
        if (!SDL_InitSubSystem(SDL_INIT_AUDIO))
        {
            LOGW("audio: SDL audio would not start (%s)", SDL_GetError());
            return;
        }
        SDL_AudioSpec spec{};
        spec.format = SDL_AUDIO_F32;
        spec.channels = int(kChannels);
        spec.freq = int(kFrequency);
        std::lock_guard<std::mutex> guard(g_streamLock);
        g_stream = SDL_OpenAudioDeviceStream(SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK, &spec, nullptr, nullptr);
        if (!g_stream)
        {
            LOGW("audio: no playback device (%s); the driver is %s", SDL_GetError(), SDL_GetCurrentAudioDriver());
            return;
        }
        SDL_AudioSpec deviceSpec{};
        int deviceFrames = 0;
        SDL_GetAudioDeviceFormat(SDL_GetAudioStreamDevice(g_stream), &deviceSpec, &deviceFrames);
        SDL_SetAudioStreamGetCallback(g_stream, Taking, nullptr);
        SDL_ResumeAudioStreamDevice(g_stream);
        g_outputOpen = true;
        LOGI("audio: %s, device %d Hz %d ch, %d-sample buffers; feeding 5.1 float at 48 kHz",
             SDL_GetCurrentAudioDriver(), deviceSpec.freq, deviceSpec.channels, deviceFrames);
#else
        LOGI("audio: built without SDL; frames are discarded");
#endif
    }

    // The worker owns a guest stack and thread block: the callback is guest
    // code and reads its thread id out of r13 like any other.
    struct GuestCaller
    {
        uint32_t stackBase = 0, stackTop = 0, pcr = 0;

        bool Prepare()
        {
            stackBase = kernel::AllocateGuest(kStackSize, 0x1000);
            const uint32_t id = kernel::NewThreadId();
            pcr = kernel::CreateThreadBlock(id);
            if (!stackBase || !pcr) return false;
            stackTop = (stackBase + kStackSize - 0x100) & ~0xFu;
            return true;
        }

        void Call(uint32_t address, uint32_t a0)
        {
            PPCFunc* fn = kernel::GuestFunction(address);
            if (!fn)
            {
                static bool warned = false;
                if (!warned) { warned = true; LOGW("audio: no recompiled function at callback %08X", address); }
                return;
            }
            PPCContext ctx{};
            ctx.r1.u32 = stackTop;
            ctx.r13.u32 = pcr;
            ctx.r3.u32 = a0;
            ctx.fpscr.loadFromHost();
            fn(ctx, guest::Base());
        }
    };

    // One call of every client whose first callback is due, each with the
    // context it registered (the title's own object); false when none is yet.
    bool CallClients(GuestCaller& caller)
    {
        uint32_t callbacks[kMaxClients], contexts[kMaxClients];
        uint32_t ready = 0;
        {
            std::lock_guard<std::mutex> guard(g_lock);
            const auto now = Clock::now();
            for (const auto& c : g_clients)
                if (c.inUse && c.next <= now) { callbacks[ready] = c.callback; contexts[ready] = c.context; ready++; }
        }
        for (uint32_t i = 0; i < ready; i++)
        {
            g_callbacks++;
            caller.Call(callbacks[i], contexts[i]);
        }
        return ready != 0;
    }

    // After each take, the callback until the queue holds that take and the
    // lead again, the size of a take being the device's to change. A take that
    // comes while the worker is still calling is answered with what the queue
    // then holds, which counts the frames already put, so a call that submits
    // refills the queue up to the target and no further. A call that submits
    // nothing is paid for by what the device asked, a credit of its bytes
    // capped at the target, so a title that submits nothing runs at the
    // device's rate, not faster.
    void PacedByTheDevice(GuestCaller& caller)
    {
        size_t credit = SIZE_MAX;
        for (;;)
        {
            size_t left, last, asked;
            {
                std::unique_lock<std::mutex> guard(g_takesLock);
                g_taken.wait(guard, [] { return g_takePending; });
                g_takePending = false;
                left = g_takeLeft;
                last = g_takeLast;
                asked = g_takeAsked;
                g_takeAsked = 0;
            }
            const size_t target = last + size_t(kLeadFrames) * kFrameBytes;
            credit = std::min(credit > SIZE_MAX - asked ? SIZE_MAX : credit + asked, target);
            const size_t shortBy = target > left ? target - left : 0;
            const size_t frames = std::min((shortBy + kFrameBytes - 1) / kFrameBytes,
                                           (credit + kFrameBytes - 1) / kFrameBytes);
            for (size_t i = 0; i < frames; i++)
            {
                const uint64_t submitted = g_frames.load(std::memory_order_relaxed);
                if (!CallClients(caller)) break;
                if (g_frames.load(std::memory_order_relaxed) == submitted)
                    credit = credit > kFrameBytes ? credit - kFrameBytes : 0;
            }
        }
    }

    // Without a device: every client on its own 5.333 ms schedule.
    void PacedByTheClock(GuestCaller& caller)
    {
        for (;;)
        {
            uint32_t callback = 0, context = 0;
            {
                std::unique_lock<std::mutex> guard(g_lock);
                Client* pick = nullptr;
                for (auto& c : g_clients)
                    if (c.inUse && (!pick || c.next < pick->next)) pick = &c;
                if (!pick)
                {
                    g_wake.wait(guard);
                    continue;
                }
                Clock::time_point due = pick->next;
                const auto now = Clock::now();
                // Behind by a lot (the process was stopped): resynchronise rather
                // than call a burst.
                if (due + 40 * kFrameInterval < now) due = now;
                if (due > now)
                {
                    g_wake.wait_until(guard, due);
                    continue;
                }
                pick->next = std::max(due, now - kFrameInterval) + kFrameInterval;
                callback = pick->callback;
                context = pick->context;
            }
            g_callbacks++;
            // The callback takes the context it registered: the title's own object.
            caller.Call(callback, context);
        }
    }

    void WorkerMain()
    {
        crash::RegisterThread("audio worker");
        GuestCaller caller;
        if (!caller.Prepare()) { LOGE("audio: could not set up the worker's guest state"); return; }
        OpenOutput();
        if (g_outputOpen) PacedByTheDevice(caller);
        else PacedByTheClock(caller);
    }

    void EnsureWorker()
    {
        if (g_workerStarted) return;
        g_workerStarted = true;
        g_worker = std::thread(WorkerMain);
        g_worker.detach();
    }
}

uint32_t apu::audio::RegisterClient(uint32_t callback, uint32_t callbackContext)
{
    std::lock_guard<std::mutex> guard(g_lock);
    for (uint32_t i = 0; i < kMaxClients; i++)
    {
        Client& c = g_clients[i];
        if (c.inUse) continue;
        c.callback = callback;
        c.context = callbackContext;
        // The first callback only once the registering call has returned and
        // the title has finished setting its object up.
        c.next = Clock::now() + std::chrono::milliseconds(100);
        c.inUse = true;
        g_registered++;
        EnsureWorker();
        g_wake.notify_all();
        LOGK("audio: client %u registered, callback %08X context %08X", i, callback, callbackContext);
        return kHandleTag | i;
    }
    LOGW("audio: no free client slot");
    return 0;
}

void apu::audio::UnregisterClient(uint32_t handle)
{
    const uint32_t i = handle & 0xFFFF;
    if ((handle & 0xFFFF0000u) != kHandleTag || i >= kMaxClients) return;
    std::lock_guard<std::mutex> guard(g_lock);
    Client& c = g_clients[i];
    if (!c.inUse) return;
    c = Client{};
    g_registered--;
    LOGK("audio: client %u unregistered", i);
}

void apu::audio::SubmitFrame(uint32_t handle, uint32_t samplesGuestAddress)
{
    const uint32_t i = handle & 0xFFFF;
    if ((handle & 0xFFFF0000u) != kHandleTag || i >= kMaxClients || !samplesGuestAddress) return;
    g_frames++;
#ifdef MW2_USE_SDL
    std::lock_guard<std::mutex> guard(g_streamLock);
    if (!g_stream) return;
    const int queued = SDL_GetAudioStreamQueued(g_stream);
    const size_t limit = size_t(kHighWaterFrames + kLeadFrames) * kFrameBytes + g_lastTake.load(std::memory_order_relaxed);
    if (queued > 0 && size_t(queued) > limit) { g_dropped++; return; }

    // Six planes of 256 big-endian floats to interleaved host floats.
    const uint32_t* in = reinterpret_cast<const uint32_t*>(guest::Base() + samplesGuestAddress);
    alignas(16) float out[kFrameFloats];
    for (uint32_t s = 0; s < kSamplesPerChannel; s++)
        for (uint32_t c = 0; c < kChannels; c++)
        {
            uint32_t bits; std::memcpy(&bits, &in[c * kSamplesPerChannel + s], 4);
            bits = __builtin_bswap32(bits);
            std::memcpy(&out[s * kChannels + c], &bits, 4);
        }
    if (!SDL_PutAudioStreamData(g_stream, out, int(kFrameBytes)))
    {
        static bool warned = false;
        if (!warned) { warned = true; LOGW("audio: put failed (%s)", SDL_GetError()); }
    }
#endif
}

extern "C" void mw2_audio_report()
{
    if (!g_callbacks) return;
    LOGI("audio: %llu callbacks, %llu frames submitted, %llu dropped, %llu underruns; %s",
         (unsigned long long)g_callbacks.load(), (unsigned long long)g_frames.load(),
         (unsigned long long)g_dropped.load(), (unsigned long long)g_underruns.load(),
         g_outputOpen ? "paced by the device" : "no device, paced by the clock");
}
