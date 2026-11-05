#pragma once
#include <cstdint>

// The audio render driver. The console's XAudio2 runs inside the title; the
// kernel's part is small: it calls a mixer callback the title registers once
// every 256 samples at 48 kHz (5.333 ms), and the callback hands back a frame
// of six channels of 256 big-endian floats through XAudioSubmitRenderDriverFrame.
// This runs that cadence on a thread of its own and pushes the frames to SDL:
// src/runtime/kernel/audio.rs, or audio.cpp in the C++ build.
extern "C" void mw2_audio_report();

namespace apu::audio
{
    // The driver handle, or 0 when no client slot is free.
    uint32_t RegisterClient(uint32_t callback, uint32_t callbackContext);
    void     UnregisterClient(uint32_t handle);
    void     SubmitFrame(uint32_t handle, uint32_t samplesGuestAddress);

    inline void Report() { mw2_audio_report(); }
}
