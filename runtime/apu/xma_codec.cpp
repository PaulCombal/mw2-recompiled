// FFmpeg's "xmaframes" decoder, for the XMA contexts of src/runtime/kernel/xma.rs: one
// codec per context, which takes one XMA frame at a time and hands back 512 samples of
// planar float per channel. The Rust build's alone; apu/xma.cpp calls FFmpeg itself.
#include "xma_codec.h"
#include "../log.h"

extern "C" {
#include <libavcodec/avcodec.h>
#include <libavutil/channel_layout.h>
#include <libavutil/error.h>
#include <libavutil/log.h>
}

#include <cstdio>
#include <cstring>

static_assert(MW2_XMA_CODEC_PADDING == AV_INPUT_BUFFER_PADDING_SIZE);

struct mw2_xma_codec
{
    const AVCodec* codec = nullptr;
    AVCodecContext* av = nullptr;
    AVPacket* packet = nullptr;
    AVFrame* frame = nullptr;
};

namespace
{
    bool g_trace = false;

    void AvLog(void*, int level, const char* fmt, va_list args)
    {
        if (level > AV_LOG_WARNING && !g_trace) return;
        if (level <= AV_LOG_WARNING && !mw2_xma_count_error()) return;
        char line[512];
        std::vsnprintf(line, sizeof line, fmt, args);
        size_t n = std::strlen(line);
        while (n && (line[n - 1] == '\n' || line[n - 1] == '\r')) line[--n] = 0;
        LOGW("xma: ffmpeg: %s", line);
    }

    void Text(int error, char* text, size_t size)
    {
        if (size) av_strerror(error, text, size);
    }
}

extern "C" void mw2_xma_codec_initialise(bool trace)
{
    g_trace = trace;
    av_log_set_callback(AvLog);
}

extern "C" mw2_xma_codec* mw2_xma_codec_new()
{
    auto* c = new mw2_xma_codec;
    c->packet = av_packet_alloc();
    c->frame = av_frame_alloc();
    c->codec = avcodec_find_decoder(AV_CODEC_ID_XMAFRAMES);
    if (!c->codec) { LOGE("xma: the xmaframes codec is missing"); return nullptr; }
    if (!c->packet || !c->frame) { LOGE("xma: out of memory for the decoder"); return nullptr; }
    return c;
}

extern "C" void mw2_xma_codec_flush(mw2_xma_codec* c)
{
    if (c->av) avcodec_flush_buffers(c->av);
}

extern "C" int mw2_xma_codec_decode(mw2_xma_codec* c, int sampleRate, int channels, const uint8_t* data,
                                    int size, float* left, float* right, char* error, size_t errorSize)
{
    if (!c->av || c->av->sample_rate != sampleRate || c->av->ch_layout.nb_channels != channels)
    {
        avcodec_free_context(&c->av);
        c->av = avcodec_alloc_context3(c->codec);
        if (!c->av) return MW2_XMA_CODEC_NO_DECODER;
        c->av->sample_rate = sampleRate;
        av_channel_layout_default(&c->av->ch_layout, channels);
        c->av->flags2 |= AV_CODEC_FLAG2_SKIP_MANUAL;
        if (avcodec_open2(c->av, c->codec, nullptr) < 0)
        {
            avcodec_free_context(&c->av);
            return MW2_XMA_CODEC_WONT_OPEN;
        }
    }

    // FFmpeg reads the packet in place, and only within data + size + padding.
    c->packet->data = const_cast<uint8_t*>(data);
    c->packet->size = size;
    int ret = avcodec_send_packet(c->av, c->packet);
    if (ret < 0) { Text(ret, error, errorSize); return MW2_XMA_CODEC_SEND_FAILED; }
    ret = avcodec_receive_frame(c->av, c->frame);
    if (ret == AVERROR(EAGAIN)) return MW2_XMA_CODEC_WARMING_UP;
    if (ret < 0) { Text(ret, error, errorSize); return MW2_XMA_CODEC_RECEIVE_FAILED; }

    constexpr size_t kPlane = MW2_XMA_CODEC_SAMPLES * sizeof(float);
    std::memcpy(left, c->frame->data[0], kPlane);
    if (channels == 2 && c->frame->data[1])
    {
        std::memcpy(right, c->frame->data[1], kPlane);
        return 2;
    }
    return 1;
}
