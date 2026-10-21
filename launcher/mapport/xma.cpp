#include "xma.h"

#include <algorithm>
#include <cmath>
#include <map>
#include <numbers>

namespace mapport
{
    namespace
    {
        constexpr int kFrame = 512;                    // samples a frame
        constexpr int kWindow = 2 * kFrame;            // samples a frame's transform takes
        constexpr int kPacket = 2048;                  // bytes a packet
        constexpr int kHeaderBits = 32;
        constexpr int kPayloadBits = kPacket * 8 - kHeaderBits;
        constexpr int kLead = kFrame + 64;             // samples before the sound: the stock files' alignment
        constexpr double kQuality = 4.0;               // band step = band level / kQuality
        constexpr double kMaskDb = 30;                 // bands this far below the frame's loudest get coarser steps
        constexpr double kFloor = 1e-4;                // smallest step (silence)
        constexpr double kGain = -32342.0;             // the MDCT scale (and sign) the decoder's inverse transform expects

        // ---- FFmpeg's code tables (libavcodec/wmaprodata.h, n6.1.1; LGPL 2.1 or
        // later, see NOTICE.md) ----------------------------------------------

        struct Huff
        {
            int symbol, length;
        };

        // Scale factor band edges in Hz (ff_wma_critical_freqs).
        constexpr int kCriticalFreq[] = {
            100, 200, 300, 400, 510, 630, 770, 920, 1080, 1270,
            1480, 1720, 2000, 2320, 2700, 3150, 3700, 4400, 5300, 6400,
            7700, 9500, 12000, 15500, 20675, 28575, 41375, 63875,
        };
        // Scale factor differences: (symbol + 60, code length), in code order (scale_table).
        constexpr Huff kScaleTable[] = {
            { 58, 5 }, { 64, 6 }, { 66, 7 }, { 65, 7 }, { 62, 5 }, { 63, 6 }, { 68, 9 }, { 69, 10 },
            { 54, 15 }, { 19, 19 }, { 20, 19 }, { 21, 19 }, { 22, 19 }, { 23, 19 }, { 24, 19 }, { 25, 19 },
            { 26, 19 }, { 27, 19 }, { 28, 19 }, { 29, 19 }, { 30, 19 }, { 31, 19 }, { 32, 19 }, { 33, 19 },
            { 34, 19 }, { 17, 19 }, { 36, 19 }, { 37, 19 }, { 38, 19 }, { 39, 19 }, { 40, 19 }, { 41, 19 },
            { 42, 19 }, { 43, 19 }, { 44, 19 }, { 45, 19 }, { 46, 19 }, { 47, 19 }, { 48, 19 }, { 49, 19 },
            { 50, 19 }, { 51, 19 }, { 52, 19 }, { 15, 19 }, { 16, 19 }, { 14, 19 }, { 13, 19 }, { 12, 19 },
            { 11, 19 }, { 10, 19 }, { 0, 19 }, { 9, 19 }, { 8, 19 }, { 7, 19 }, { 6, 19 }, { 5, 19 },
            { 4, 19 }, { 55, 13 }, { 70, 13 }, { 3, 19 }, { 2, 19 }, { 1, 19 }, { 35, 19 }, { 71, 19 },
            { 72, 19 }, { 73, 19 }, { 74, 19 }, { 75, 19 }, { 76, 19 }, { 77, 19 }, { 78, 19 }, { 79, 19 },
            { 80, 19 }, { 81, 19 }, { 82, 19 }, { 83, 19 }, { 84, 19 }, { 85, 19 }, { 86, 19 }, { 87, 19 },
            { 88, 19 }, { 89, 19 }, { 90, 19 }, { 91, 19 }, { 92, 19 }, { 93, 19 }, { 94, 19 }, { 95, 19 },
            { 96, 19 }, { 97, 19 }, { 98, 19 }, { 99, 19 }, { 100, 19 }, { 101, 19 }, { 102, 19 }, { 103, 19 },
            { 104, 19 }, { 105, 19 }, { 106, 19 }, { 107, 19 }, { 108, 19 }, { 109, 19 }, { 110, 19 }, { 111, 19 },
            { 112, 19 }, { 113, 19 }, { 114, 19 }, { 115, 19 }, { 116, 19 }, { 117, 19 }, { 118, 19 }, { 119, 19 },
            { 120, 19 }, { 18, 18 }, { 53, 16 }, { 56, 11 }, { 57, 8 }, { 67, 7 }, { 61, 3 }, { 59, 2 },
            { 60, 1 },
        };
        // Coefficient table 0: code lengths and symbols, in code order (coef0_lens, coef0_syms).
        constexpr uint8_t kCoef0Lens[] = {
            2, 9, 14, 14, 13, 12, 13, 14, 15, 15, 12, 10, 10, 10, 13, 14, 15, 15, 12, 11, 13, 14, 14, 13,
            15, 15, 14, 12, 12, 8, 10, 10, 15, 15, 14, 13, 14, 14, 13, 15, 20, 20, 19, 21, 21, 20, 19, 17,
            17, 18, 18, 15, 15, 13, 12, 14, 15, 15, 14, 15, 15, 12, 11, 6, 7, 8, 9, 13, 13, 13, 14, 14,
            11, 10, 7, 8, 14, 14, 14, 14, 12, 13, 13, 12, 12, 12, 11, 9, 13, 14, 14, 12, 11, 11, 11, 9,
            8, 7, 14, 15, 15, 14, 14, 12, 13, 15, 16, 17, 17, 14, 12, 12, 12, 15, 15, 14, 14, 14, 13, 13,
            9, 9, 11, 11, 10, 7, 6, 13, 15, 15, 14, 14, 14, 13, 14, 15, 15, 13, 14, 14, 14, 14, 10, 9,
            10, 10, 11, 11, 10, 8, 9, 13, 14, 14, 12, 11, 14, 15, 15, 13, 12, 14, 14, 14, 14, 13, 14, 14,
            3, 5, 8, 10, 10, 15, 15, 14, 14, 16, 16, 15, 12, 11, 11, 11, 7, 8, 8, 9, 12, 13, 13, 12,
            14, 15, 15, 13, 10, 11, 11, 13, 14, 14, 13, 14, 14, 11, 10, 13, 15, 15, 14, 12, 11, 4, 6, 6,
            8, 12, 12, 12, 13, 13, 12, 13, 13, 14, 14, 13, 13, 13, 9, 7, 9, 11, 14, 14, 13, 14, 14, 13,
            10, 8, 7, 5, 9, 12, 13, 14, 15, 15, 12, 12, 10, 14, 14, 13, 12, 13, 14, 14, 12, 13, 13, 12,
            12, 12, 9, 7, 6, 3, 4, 4,
        };
        constexpr uint16_t kCoef0Syms[] = {
            2, 25, 111, 94, 69, 58, 87, 93, 136, 135, 59, 37, 34, 36, 82, 182,
            120, 138, 195, 45, 168, 216, 178, 86, 140, 219, 186, 162, 239, 18, 156, 35,
            127, 236, 109, 85, 180, 253, 88, 147, 268, 264, 256, 266, 270, 262, 260, 248,
            246, 252, 258, 137, 189, 230, 64, 179, 146, 208, 101, 118, 238, 163, 46, 9,
            153, 0, 26, 247, 169, 76, 202, 131, 194, 38, 13, 19, 132, 106, 191, 97,
            65, 198, 77, 62, 66, 164, 48, 27, 81, 183, 102, 60, 47, 49, 159, 227,
            20, 14, 112, 263, 144, 217, 104, 63, 79, 209, 269, 250, 254, 203, 241, 196,
            61, 220, 148, 124, 185, 100, 80, 78, 193, 28, 50, 235, 41, 1, 10, 171,
            226, 150, 103, 114, 115, 170, 105, 211, 149, 249, 108, 188, 107, 255, 231, 155,
            42, 40, 55, 160, 39, 21, 29, 215, 234, 184, 228, 51, 116, 142, 145, 172,
            165, 181, 130, 113, 117, 89, 128, 204, 3, 7, 154, 157, 43, 141, 265, 133,
            225, 271, 244, 221, 74, 54, 56, 52, 15, 222, 22, 30, 83, 199, 173, 73,
            123, 210, 143, 175, 44, 53, 237, 174, 139, 134, 110, 218, 129, 161, 213, 177,
            267, 151, 125, 67, 223, 5, 11, 192, 23, 214, 243, 166, 200, 176, 68, 224,
            187, 257, 261, 232, 96, 251, 31, 16, 32, 57, 207, 121, 91, 126, 119, 99,
            158, 24, 212, 8, 33, 70, 92, 205, 240, 242, 75, 197, 233, 259, 190, 98,
            71, 201, 122, 206, 72, 90, 95, 84, 167, 245, 229, 17, 12, 4, 152, 6,
        };
        // Coefficient table 1: (symbol, code length), in code order (coef1_table).
        constexpr Huff kCoef1Table[] = {
            { 2, 2 }, { 3, 3 }, { 102, 3 }, { 4, 4 }, { 148, 6 }, { 134, 9 }, { 171, 10 }, { 18, 10 },
            { 11, 8 }, { 159, 8 }, { 14, 9 }, { 156, 14 }, { 235, 15 }, { 61, 15 }, { 38, 13 }, { 153, 13 },
            { 48, 14 }, { 49, 14 }, { 23, 11 }, { 203, 13 }, { 208, 19 }, { 204, 19 }, { 129, 18 }, { 94, 17 },
            { 87, 16 }, { 62, 15 }, { 174, 15 }, { 147, 15 }, { 29, 12 }, { 191, 12 }, { 64, 15 }, { 65, 15 },
            { 146, 14 }, { 164, 13 }, { 142, 5 }, { 132, 4 }, { 103, 5 }, { 154, 7 }, { 165, 9 }, { 181, 11 },
            { 109, 12 }, { 30, 12 }, { 86, 16 }, { 92, 16 }, { 239, 15 }, { 138, 14 }, { 39, 13 }, { 50, 14 },
            { 115, 15 }, { 238, 21 }, { 228, 21 }, { 236, 21 }, { 222, 21 }, { 216, 20 }, { 226, 20 }, { 196, 18 },
            { 192, 17 }, { 120, 16 }, { 221, 14 }, { 51, 14 }, { 24, 11 }, { 143, 8 }, { 7, 6 }, { 9, 7 },
            { 152, 10 }, { 136, 12 }, { 160, 12 }, { 241, 15 }, { 66, 15 }, { 168, 14 }, { 219, 14 }, { 113, 14 },
            { 193, 12 }, { 19, 10 }, { 173, 10 }, { 105, 8 }, { 149, 9 }, { 15, 9 }, { 205, 13 }, { 207, 13 },
            { 125, 17 }, { 190, 17 }, { 182, 16 }, { 68, 15 }, { 70, 15 }, { 67, 15 }, { 137, 13 }, { 31, 12 },
            { 223, 14 }, { 116, 15 }, { 210, 19 }, { 220, 19 }, { 198, 18 }, { 126, 17 }, { 88, 16 }, { 41, 13 },
            { 25, 11 }, { 40, 13 }, { 73, 15 }, { 243, 15 }, { 53, 14 }, { 195, 12 }, { 183, 11 }, { 225, 14 },
            { 52, 14 }, { 71, 15 }, { 121, 16 }, { 89, 16 }, { 170, 14 }, { 55, 14 }, { 69, 15 }, { 83, 15 },
            { 209, 13 }, { 108, 11 }, { 32, 12 }, { 54, 14 }, { 122, 16 }, { 184, 16 }, { 176, 15 }, { 42, 13 },
            { 12, 8 }, { 161, 8 }, { 6, 5 }, { 167, 9 }, { 106, 9 }, { 20, 10 }, { 145, 12 }, { 111, 13 },
            { 43, 13 }, { 26, 11 }, { 175, 10 }, { 107, 10 }, { 34, 12 }, { 33, 12 }, { 197, 12 }, { 74, 15 },
            { 128, 17 }, { 232, 20 }, { 212, 20 }, { 224, 19 }, { 202, 18 }, { 90, 16 }, { 57, 14 }, { 227, 14 },
            { 97, 16 }, { 93, 16 }, { 140, 15 }, { 185, 11 }, { 27, 11 }, { 16, 9 }, { 158, 11 }, { 211, 13 },
            { 56, 14 }, { 117, 15 }, { 72, 15 }, { 166, 13 }, { 91, 16 }, { 95, 16 }, { 80, 15 }, { 101, 16 },
            { 194, 17 }, { 127, 17 }, { 82, 15 }, { 21, 10 }, { 144, 10 }, { 177, 10 }, { 151, 6 }, { 10, 7 },
            { 157, 7 }, { 8, 6 }, { 5, 4 }, { 13, 8 }, { 0, 9 }, { 213, 13 }, { 46, 13 }, { 199, 12 },
            { 35, 12 }, { 162, 12 }, { 135, 10 }, { 169, 9 }, { 45, 13 }, { 59, 14 }, { 114, 14 }, { 44, 13 },
            { 188, 16 }, { 186, 16 }, { 75, 15 }, { 79, 15 }, { 118, 15 }, { 187, 11 }, { 112, 13 }, { 139, 14 },
            { 178, 15 }, { 81, 15 }, { 110, 12 }, { 28, 11 }, { 163, 8 }, { 133, 6 }, { 104, 6 }, { 17, 9 },
            { 22, 10 }, { 229, 14 }, { 172, 14 }, { 217, 13 }, { 201, 12 }, { 36, 12 }, { 218, 20 }, { 242, 22 },
            { 240, 22 }, { 234, 21 }, { 230, 19 }, { 206, 18 }, { 200, 18 }, { 214, 18 }, { 130, 17 }, { 131, 17 },
            { 141, 15 }, { 84, 15 }, { 76, 15 }, { 215, 13 }, { 58, 14 }, { 231, 14 }, { 233, 14 }, { 180, 15 },
            { 77, 15 }, { 37, 12 }, { 189, 11 }, { 179, 10 }, { 155, 10 }, { 47, 13 }, { 96, 16 }, { 99, 16 },
            { 119, 15 }, { 63, 14 }, { 237, 14 }, { 78, 15 }, { 85, 15 }, { 60, 14 }, { 98, 16 }, { 100, 16 },
            { 124, 16 }, { 123, 16 }, { 150, 11 }, { 1, 7 },
        };
        // Each symbol's run and level (coef0_run, coef0_level, coef1_run, coef1_level).
        constexpr uint8_t kCoef0Run[] = {
            0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
            22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45,
            46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69,
            70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93,
            94, 95, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116, 117,
            118, 119, 120, 121, 122, 123, 124, 125, 126, 127, 128, 129, 130, 131, 132, 133, 134, 135, 136, 137, 138, 139, 140, 141,
            142, 143, 144, 145, 146, 147, 148, 149, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
            16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39,
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 0, 1, 2, 3,
            4, 5, 6, 7, 8, 9, 0, 1, 2, 3, 4, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0,
            1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0,
            1, 0, 1, 0, 1, 0, 1, 0,
        };
        constexpr uint8_t kCoef0Level[] = {
            0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
            2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
            3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4,
            4, 4, 4, 4, 4, 4, 5, 5, 5, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12,
            12, 13, 13, 14, 14, 15, 15, 16, 16, 17, 17, 18, 18, 19, 19, 20, 20, 21, 21, 22, 22, 23, 23, 24,
            24, 25, 25, 26, 26, 27, 27, 28,
        };
        constexpr uint8_t kCoef1Run[] = {
            0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
            22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45,
            46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69,
            70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93,
            94, 95, 96, 97, 98, 99, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17,
            18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0, 1,
            2, 3, 4, 5, 0, 1, 2, 0, 1, 2, 0, 1, 2, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0,
            1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0,
            1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0,
            1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0,
            1, 0, 0, 0,
        };
        constexpr uint8_t kCoef1Level[] = {
            0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
            2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4,
            4, 4, 4, 4, 5, 5, 5, 6, 6, 6, 7, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
            13, 14, 14, 15, 15, 16, 16, 17, 17, 18, 18, 19, 19, 20, 20, 21, 21, 22, 22, 23, 23, 24, 24, 25,
            25, 26, 26, 27, 27, 28, 28, 29, 29, 30, 30, 31, 31, 32, 32, 33, 33, 34, 34, 35, 35, 36, 36, 37,
            37, 38, 38, 39, 39, 40, 40, 41, 41, 42, 42, 43, 43, 44, 44, 45, 45, 46, 46, 47, 47, 48, 48, 49,
            49, 50, 51, 52,
        };

        // ---- bits -----------------------------------------------------------

        // Bits written most significant first.
        class Bits
        {
        public:
            void Put(uint64_t v, int n)
            {
                for (int i = n - 1; i >= 0; i--) PutBit((v >> i) & 1);
            }
            void Append(const Bits& b)
            {
                for (size_t i = 0; i < b.size; i++) PutBit(b.Bit(i));
            }
            size_t Size() const { return size; }
            bool Bit(size_t i) const { return (bytes[i / 8] >> (7 - i % 8)) & 1; }
            // Zeros up to a whole number of `n` bits.
            void PadTo(size_t n)
            {
                while (size % n) PutBit(false);
            }
            const std::vector<uint8_t>& Bytes() const { return bytes; }

        private:
            void PutBit(bool b)
            {
                if (size % 8 == 0) bytes.push_back(0);
                if (b) bytes.back() |= uint8_t(0x80 >> (size % 8));
                size++;
            }
            std::vector<uint8_t> bytes;
            size_t size = 0;
        };

        struct Code
        {
            uint32_t bits = 0;
            int length = 0;
        };

        // Codes for the symbols of a table of (symbol, length) in code order,
        // as FFmpeg's ff_vlc_init_from_lengths hands them out.
        std::vector<Code> Canonical(const std::vector<Huff>& table)
        {
            std::vector<Code> codes;
            uint64_t code = 0;
            for (const auto& [symbol, length] : table)
                if (length > 0)
                {
                    const size_t at = size_t(symbol);
                    if (codes.size() <= at) codes.resize(at + 1);
                    codes[at] = { uint32_t(code >> (32 - length)), length };
                    code += uint64_t(1) << (32 - length);
                }
            return codes;
        }

        // A coefficient table: each symbol's code, and the symbol of a run of
        // zeros and the level after it (2 and up; 0 escapes, 1 ends a block).
        struct CoefTable
        {
            std::vector<Code> codes;
            std::map<std::pair<int, int>, int> runLevel;
        };

        CoefTable MakeCoefTable(const std::vector<Huff>& table, std::span<const uint8_t> runs, std::span<const uint8_t> levels)
        {
            CoefTable t{ Canonical(table), {} };
            for (size_t sym = 2; sym < runs.size(); sym++) t.runLevel.emplace(std::pair(runs[sym], levels[sym]), int(sym));
            return t;
        }

        const std::vector<Code>& ScaleCodes()
        {
            static const std::vector<Code> codes = Canonical({ std::begin(kScaleTable), std::end(kScaleTable) });
            return codes;   // by difference + 60
        }

        const CoefTable& Coefs(int t)
        {
            static const CoefTable tables[2] = {
                [] {
                    std::vector<Huff> table;
                    for (size_t i = 0; i < std::size(kCoef0Lens); i++) table.push_back({ kCoef0Syms[i], kCoef0Lens[i] });
                    return MakeCoefTable(table, kCoef0Run, kCoef0Level);
                }(),
                MakeCoefTable({ std::begin(kCoef1Table), std::end(kCoef1Table) }, kCoef1Run, kCoef1Level),
            };
            return tables[t];
        }

        void Put(Bits& b, const Code& c) { b.Put(c.bits, c.length); }

        // ---- a frame --------------------------------------------------------

        // Scale factor band edges of a 512-sample subframe, as the decoder
        // works them out.
        std::vector<int> BandOffsets(uint32_t rate)
        {
            const int r = rate > 44100 ? 48000 : rate > 32000 ? 44100 : rate > 24000 ? 32000 : 24000;
            std::vector<int> offs{ 0 };
            for (int f : kCriticalFreq)
            {
                const int o = ((kFrame * 2 * f) / r + 2) & ~3;
                if (o > offs.back()) offs.push_back(o);
                if (o >= kFrame) break;
            }
            offs.back() = kFrame;
            return offs;
        }

        // A sum added up pairwise, eight at a time: the order a band's level
        // comes out of decides its last bits.
        double PairwiseSum(const double* a, size_t n)
        {
            if (n < 8)
            {
                double s = 0;
                for (size_t i = 0; i < n; i++) s += a[i];
                return s;
            }
            if (n <= 128)
            {
                double r[8];
                std::copy(a, a + 8, r);
                size_t i = 8;
                for (; i < n - n % 8; i += 8)
                    for (int j = 0; j < 8; j++) r[j] += a[i + j];
                double s = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
                for (; i < n; i++) s += a[i];
                return s;
            }
            size_t half = n / 2;
            half -= half % 8;
            return PairwiseSum(a, half) + PairwiseSum(a + half, n - half);
        }

        // A value after the prefix code saying how many bits it takes.
        void Prefixed(Bits& b, uint32_t prefix, int prefixBits, uint64_t v, int bits)
        {
            b.Put(prefix, prefixBits);
            b.Put(v, bits);
        }

        void Large(Bits& b, uint32_t v)
        {
            if (v < 1u << 8) Prefixed(b, 0, 1, v, 8);
            else if (v < 1u << 16) Prefixed(b, 2, 2, v, 16);
            else if (v < 1u << 24) Prefixed(b, 6, 3, v, 24);
            else Prefixed(b, 7, 3, v, 31);
        }

        // Run-level coding of a frame's whole-number coefficients with
        // coefficient table t.
        Bits Coefficients(const std::vector<int64_t>& q, int t)
        {
            const CoefTable& table = Coefs(t);
            Bits b;
            int64_t prev = -1;
            for (int64_t i = 0; i < int64_t(q.size()); i++)
            {
                if (!q[i]) continue;
                const int64_t run = i - prev - 1, level = std::abs(q[i]);
                if (auto sym = table.runLevel.find({ int(run), int(level) }); sym != table.runLevel.end())
                    Put(b, table.codes[size_t(sym->second)]);
                else
                {
                    Put(b, table.codes[0]);   // escape: the level, then the run
                    Large(b, uint32_t(level));
                    if (run == 0) b.Put(0, 1);
                    else if (run <= 4) Prefixed(b, 2, 2, uint64_t(run - 1), 2);
                    else Prefixed(b, 6, 3, uint64_t(run - 4), 9);
                }
                b.Put(q[i] > 0, 1);
                prev = i;
            }
            if (prev + 1 < int64_t(q.size())) Put(b, table.codes[1]);   // end of block
            return b;
        }

        // One frame's bits (after its length, before its two end bits) for
        // its 512 coefficients.
        Bits Frame(const double* X, const std::vector<int>& offs, bool first)
        {
            const double mask = std::pow(10.0, -kMaskDb / 20.0);
            std::vector<double> levels;
            for (size_t i = 0; i + 1 < offs.size(); i++)
            {
                double squares[kFrame];
                const int a = offs[i], n = offs[i + 1] - a;
                for (int k = 0; k < n; k++) squares[k] = X[a + k] * X[a + k];
                levels.push_back(std::sqrt(PairwiseSum(squares, size_t(n)) / n));
            }
            const double top = *std::max_element(levels.begin(), levels.end());
            std::vector<int> d;
            for (double level : levels)
                d.push_back(int(std::nearbyint(20 * std::log10(std::max({ level / kQuality, top * mask, kFloor })))));
            for (size_t i = 1; i < d.size(); i++)   // a scale factor may move by at most 60 a band
                d[i] = std::clamp(d[i], d[i - 1] - 60, d[i - 1] + 60);
            const int Q = *std::max_element(d.begin(), d.end());
            std::vector<int64_t> q(kFrame);
            bool any = false;
            for (size_t i = 0; i < d.size(); i++)
            {
                const double step = std::pow(10.0, d[i] / 20.0);
                for (int k = offs[i]; k < offs[i + 1]; k++) any |= (q[size_t(k)] = int64_t(std::nearbyint(X[k] / step))) != 0;
            }
            Bits f;
            f.Put(1, 1);   // all channels share the subframe layout
            f.Put(0, 1);   // one 512-sample subframe
            f.Put(0, 8);   // dynamic range gain
            if (first)
            {
                // Trim 512 samples at the start, none at the end.
                f.Put(1, 1);
                f.Put(1, 1);
                f.Put(512, 10);
                f.Put(0, 1);
            }
            else f.Put(0, 1);   // no trims
            f.Put(0, 1);   // no extended subframe header
            f.Put(0, 1);   // reserved
            if (!any)
            {
                f.Put(0, 1);   // no coefficients
                return f;
            }
            f.Put(1, 1);
            f.Put(1, 1);   // 0 vector coded coefficients: all run-level coded
            f.Put(0, 8);
            const int step = Q - 90;
            if (step >= -31 && step <= 30) f.Put(uint64_t(step), 6);
            else
            {
                int rest = step < 0 ? 58 - Q : Q - 121;
                f.Put(step < 0 ? uint64_t(-32) : 31, 6);
                for (; rest >= 31; rest -= 31) f.Put(31, 5);
                f.Put(uint64_t(rest), 5);
            }
            f.Put(0, 2);   // scale factor step 1 (dB)
            int val = 45;
            for (int v : d)
            {
                const int s = v - Q + 60;
                Put(f, ScaleCodes().at(size_t(s - val + 60)));
                val = s;
            }
            const Bits c0 = Coefficients(q, 0), c1 = Coefficients(q, 1);
            f.Put(c0.Size() <= c1.Size() ? 0 : 1, 1);
            f.Append(c0.Size() <= c1.Size() ? c0 : c1);
            return f;
        }

        // The transform: a frame's 512 coefficients are its 1,024 samples,
        // windowed, times this matrix. Each coefficient adds its products up
        // in sample order; kept a sample's column at a
        // time, all 512 sums go forward together.
        struct Transform
        {
            std::vector<double> window, mdct;   // mdct: 1,024 columns of 512
            Transform() : window(kWindow), mdct(size_t(kFrame) * kWindow)
            {
                const double pi = std::numbers::pi;
                for (int n = 0; n < kWindow; n++) window[size_t(n)] = std::sin(pi * (n + 0.5) / kWindow);
                for (int n = 0; n < kWindow; n++)
                    for (int k = 0; k < kFrame; k++)
                        mdct[size_t(n) * kFrame + size_t(k)] = std::cos(pi / kFrame * (n + 0.5 + kFrame / 2) * (k + 0.5)) * kGain;
            }
            void Apply(const double* samples, double* out) const
            {
                std::fill(out, out + kFrame, 0.0);
                for (int n = 0; n < kWindow; n++)
                {
                    const double w = samples[n] * window[size_t(n)];
                    const double* column = &mdct[size_t(n) * kFrame];
                    for (int k = 0; k < kFrame; k++) out[k] += w * column[k];
                }
            }
        };

        // Frames to packets: each frame is its 15-bit length, its bits, a 1 and
        // a trailer bit (1 when the next frame ends in the same packet too).
        Xma Pack(const std::vector<Bits>& frames, uint32_t samples, uint32_t rate)
        {
            std::vector<uint64_t> starts;
            uint64_t pos = 0;
            for (const Bits& f : frames)
            {
                starts.push_back(pos);
                pos += 15 + f.Size() + 2;
            }
            auto end = [&](size_t i) { return i + 1 < starts.size() ? starts[i + 1] : pos; };
            Bits stream;
            for (size_t i = 0; i < frames.size(); i++)
            {
                const bool more = i + 1 < frames.size() && (end(i) - 1) / kPayloadBits == (end(i + 1) - 1) / kPayloadBits;
                stream.Put(15 + frames[i].Size() + 2, 15);
                stream.Append(frames[i]);
                stream.Put(1, 1);
                stream.Put(more, 1);
            }
            stream.PadTo(kPayloadBits);
            Xma out;
            out.samples = samples;
            out.rate = rate;
            const size_t packets = stream.Size() / kPayloadBits;
            size_t k = 0;
            for (size_t p = 0; p < packets; p++)
            {
                const uint64_t a = p * kPayloadBits;
                while (k < starts.size() && starts[k] < a) k++;
                out.seek.push_back(uint32_t(kFrame * k));
                // The bits of the frame carried over from the packet before.
                const uint64_t prev = k < starts.size() && starts[k] < a + kPayloadBits ? starts[k] - a : kPayloadBits;
                const uint32_t header = uint32_t((p & 15) << 28) | (2u << 26) | uint32_t(std::min<uint64_t>(prev, 0x7FFF) << 11);
                for (int s = 24; s >= 0; s -= 8) out.data.push_back(uint8_t(header >> s));
                const auto payload = stream.Bytes().begin() + std::ptrdiff_t(a / 8);
                out.data.insert(out.data.end(), payload, payload + kPayloadBits / 8);
            }
            out.endBit = pos + kHeaderBits * ((pos - 1) / kPayloadBits + 1);
            return out;
        }
    }

    Xma EncodeXma(std::span<const int16_t> pcm, uint32_t rate, int channels)
    {
        const size_t n = pcm.size() / size_t(channels);
        const size_t frames = (kLead + n + kFrame - 1) / kFrame + 1;
        std::vector<double> padded(frames * kFrame + kFrame);
        for (size_t i = 0; i < n; i++)
        {
            double s = 0;
            for (int c = 0; c < channels; c++) s += pcm[i * size_t(channels) + size_t(c)] / 32768.0;
            padded[kLead + i] = channels > 1 ? s / channels : s;
        }
        static const Transform transform;
        const std::vector<int> offs = BandOffsets(rate);
        std::vector<Bits> bits;
        double X[kFrame];
        for (size_t i = 0; i < frames; i++)
        {
            transform.Apply(&padded[i * kFrame], X);
            bits.push_back(Frame(X, offs, i == 0));
        }
        return Pack(bits, uint32_t(n), rate);
    }

    namespace
    {
        uint32_t Le(std::span<const uint8_t> b, size_t at, int size)
        {
            uint32_t v = 0;
            for (int i = size - 1; i >= 0; i--) v = v << 8 | b[at + size_t(i)];
            return v;
        }

        // Whole frames of 16-bit samples.
        Pcm Samples(std::span<const uint8_t> data, uint32_t rate, int channels)
        {
            Pcm pcm{ {}, rate, channels };
            const size_t frame = 2 * size_t(channels);
            for (size_t i = 0; i + 1 < data.size() / frame * frame; i += 2) pcm.samples.push_back(int16_t(Le(data, i, 2)));
            return pcm;
        }
    }

    SoundPcm ReadSound(std::span<const uint8_t> raw, std::optional<PcmFormat> plain)
    {
        auto is = [&](size_t at, const char* tag) { return raw.size() >= at + 4 && std::equal(tag, tag + 4, raw.begin() + std::ptrdiff_t(at)); };
        if (is(0, "RIFF") && is(8, "WAVE"))
        {
            std::optional<std::span<const uint8_t>> fmt, data;
            for (size_t i = 12; i + 8 <= raw.size();)
            {
                const size_t length = Le(raw, i + 4, 4);
                const auto body = raw.subspan(i + 8, std::min(length, raw.size() - (i + 8)));
                if (is(i, "fmt ")) fmt = body;
                else if (is(i, "data")) data = body;
                i += 8 + length + (length & 1);
            }
            if (!fmt || !data || fmt->size() < 16) return {};
            uint32_t tag = Le(*fmt, 0, 2);
            const int channels = int(Le(*fmt, 2, 2));
            const uint32_t rate = Le(*fmt, 4, 4), bits = Le(*fmt, 14, 2);
            if (tag == 0xFFFE && fmt->size() >= 26) tag = Le(*fmt, 24, 2);   // WAVE_FORMAT_EXTENSIBLE: its subformat
            if (tag != 1 || bits != 16 || (channels != 1 && channels != 2) || !rate || data->size() < 2 * size_t(channels))
                return { std::nullopt, true };
            return { Samples(*data, rate, channels), false };
        }
        if (plain) return { Samples(raw, plain->rate, plain->channels), false };
        return { std::nullopt, true };
    }
}
