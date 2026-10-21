#pragma once
// The .iwi wavelet pictures (formats 6 to 10): Infinity Ward's own
// compression. A picture is stored as its smallest mip level (raw samples),
// then, for each larger level, the detail coefficients that turn the level
// below into the next one (an inverse Haar-style lifting step per 2x2
// block), Huffman coded with three fixed codebooks, read least significant
// bit first. After OpenAssetTools' IwiWaveletDecoder (the iw4x-x64/oat
// fork).
#include <cstdint>
#include <vector>

namespace mapport
{
    // Every mip level of a width x height picture with `channels`
    // interleaved 8-bit channels (4: B, G, R, A; 3: B, G, R; 2: L, A; 1: one),
    // largest first, from `data` (the file after its header). Throws Error.
    std::vector<std::vector<uint8_t>> DecodeWavelet(const uint8_t* data, size_t size, int width, int height, int channels);
}
