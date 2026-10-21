#pragma once
// An XMA encoder for the 360's sounds: 16-bit PCM in, the packets the game
// plays out. The 360 game plays XMA only, and a map's own sounds come as PC
// .wav files.
//
// XMA is WMA Pro's bitstream in 2,048-byte packets; this writes the subset
// FFmpeg's decoder (libavcodec/wmaprodec.c) reads and the stock files use:
// frames of 512 samples, each one subframe (a 1,024-point MDCT with a sine
// window), per-band scale factors, the coefficients run-level coded.
#include <cstdint>
#include <optional>
#include <span>
#include <vector>

namespace mapport
{
    struct Xma
    {
        std::vector<uint8_t> data;       // the packets
        std::vector<uint32_t> seek;      // samples before each packet
        uint64_t endBit = 0;             // where the last frame ends, packet headers counted
        uint32_t samples = 0, rate = 0;
        uint32_t Ms() const { return uint32_t(uint64_t(samples) * 1000 / rate); }
    };

    // Encodes interleaved 16-bit samples of one or two channels (stereo is
    // mixed to mono, the only layout written).
    Xma EncodeXma(std::span<const int16_t> pcm, uint32_t rate, int channels);

    // 16-bit PCM: interleaved samples, their rate and channel count.
    struct Pcm
    {
        std::vector<int16_t> samples;
        uint32_t rate = 0;
        int channels = 1;
    };

    // What a PC sound file holds for the encoder.
    struct SoundPcm
    {
        std::optional<Pcm> pcm;
        bool compressed = false;   // another format (ADPCM, 8-bit, MP3, ...), which isn't decoded
    };

    struct PcmFormat
    {
        uint32_t rate;
        int channels;
    };

    // The audio of a .wav file (RIFF, mono or stereo), or else of plain
    // 16-bit samples in the format given (a PC LoadedSound's).
    SoundPcm ReadSound(std::span<const uint8_t> raw, std::optional<PcmFormat> plain = std::nullopt);
}
