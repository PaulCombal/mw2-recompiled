#pragma once
#include <cstddef>
#include <cstdint>

// FFmpeg's xmaframes decoder behind C calls, for src/runtime/kernel/xma.rs.
extern "C" {
    struct mw2_xma_codec;

    enum
    {
        MW2_XMA_CODEC_SAMPLES = 512,
        MW2_XMA_CODEC_PADDING = 64,   // bytes FFmpeg may read past a packet's end
        MW2_XMA_CODEC_WARMING_UP = 0,
        MW2_XMA_CODEC_NO_DECODER = -1,
        MW2_XMA_CODEC_WONT_OPEN = -2,
        MW2_XMA_CODEC_SEND_FAILED = -3,
        MW2_XMA_CODEC_RECEIVE_FAILED = -4,
    };

    // FFmpeg's log goes to ours: warnings count as decoder errors, the rest only when
    // tracing.
    void mw2_xma_codec_initialise(bool trace);
    // Null when FFmpeg has no xmaframes decoder or no memory.
    mw2_xma_codec* mw2_xma_codec_new();
    void mw2_xma_codec_flush(mw2_xma_codec* codec);
    // Decodes one frame of `data` (`size` bytes followed by MW2_XMA_CODEC_PADDING more),
    // reopening the decoder when the rate or channels change. The channels written to
    // `left` and `right` (512 floats each), or one of the codes above, with FFmpeg's text
    // of a failed send or receive in `error`.
    int mw2_xma_codec_decode(mw2_xma_codec* codec, int sampleRate, int channels, const uint8_t* data,
                             int size, float* left, float* right, char* error, size_t errorSize);

    // src/runtime/kernel/xma.rs: true while the decoder's errors are still logged.
    bool mw2_xma_count_error();
}
