// An XMA encoder for the 360's sounds: 16-bit PCM in, the packets the game plays out. The
// 360 game plays XMA only, and a map's own sounds come as PC .wav files.
//
// XMA is WMA Pro's bitstream in 2,048-byte packets; this writes the subset FFmpeg's decoder
// (libavcodec/wmaprodec.c) reads and the stock files use: frames of 512 samples, each one
// subframe (a 1,024-point MDCT with a sine window), per-band scale factors, the coefficients
// run-level coded.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::num::truncate;

const FRAME: usize = 512; // samples a frame
const WINDOW: usize = 2 * FRAME; // samples a frame's transform takes
const PACKET: u64 = 2048; // bytes a packet
const HEADER_BITS: u64 = 32;
const PAYLOAD_BITS: u64 = PACKET * 8 - HEADER_BITS;
const LEAD: usize = FRAME + 64; // samples before the sound: the stock files' alignment
const QUALITY: f64 = 4.0; // band step = band level / QUALITY
const MASK_DB: f64 = 30.0; // bands this far below the frame's loudest get coarser steps
const FLOOR: f64 = 1e-4; // smallest step (silence)
const GAIN: f64 = -32342.0; // the MDCT scale (and sign) the decoder's inverse transform expects

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Xma {
    /// The packets.
    pub data: Vec<u8>,
    /// Samples before each packet.
    pub seek: Vec<u32>,
    /// Where the last frame ends, packet headers counted.
    pub end_bit: u64,
    pub samples: u32,
    pub rate: u32,
}

impl Xma {
    pub fn ms(&self) -> u32 {
        (u64::from(self.samples) * 1000).checked_div(u64::from(self.rate)).unwrap_or(0) as u32
    }
}

// ---- FFmpeg's code tables (libavcodec/wmaprodata.h, n6.1.1; LGPL 2.1 or later, see
// NOTICE.md) ------------------------------------------------------------------------------

/// A code table entry: (symbol, code length).
type Huff = (u16, u8);

/// Scale factor band edges in Hz (ff_wma_critical_freqs).
const CRITICAL_FREQ: [i32; 28] = [
    100, 200, 300, 400, 510, 630, 770, 920, 1080, 1270, 1480, 1720, 2000, 2320, 2700, 3150, 3700, 4400, 5300, 6400,
    7700, 9500, 12000, 15500, 20675, 28575, 41375, 63875,
];

/// Scale factor differences: (symbol + 60, code length), in code order (scale_table).
#[rustfmt::skip]
const SCALE_TABLE: [Huff; 121] = [
    (58, 5), (64, 6), (66, 7), (65, 7), (62, 5), (63, 6), (68, 9), (69, 10),
    (54, 15), (19, 19), (20, 19), (21, 19), (22, 19), (23, 19), (24, 19), (25, 19),
    (26, 19), (27, 19), (28, 19), (29, 19), (30, 19), (31, 19), (32, 19), (33, 19),
    (34, 19), (17, 19), (36, 19), (37, 19), (38, 19), (39, 19), (40, 19), (41, 19),
    (42, 19), (43, 19), (44, 19), (45, 19), (46, 19), (47, 19), (48, 19), (49, 19),
    (50, 19), (51, 19), (52, 19), (15, 19), (16, 19), (14, 19), (13, 19), (12, 19),
    (11, 19), (10, 19), (0, 19), (9, 19), (8, 19), (7, 19), (6, 19), (5, 19),
    (4, 19), (55, 13), (70, 13), (3, 19), (2, 19), (1, 19), (35, 19), (71, 19),
    (72, 19), (73, 19), (74, 19), (75, 19), (76, 19), (77, 19), (78, 19), (79, 19),
    (80, 19), (81, 19), (82, 19), (83, 19), (84, 19), (85, 19), (86, 19), (87, 19),
    (88, 19), (89, 19), (90, 19), (91, 19), (92, 19), (93, 19), (94, 19), (95, 19),
    (96, 19), (97, 19), (98, 19), (99, 19), (100, 19), (101, 19), (102, 19), (103, 19),
    (104, 19), (105, 19), (106, 19), (107, 19), (108, 19), (109, 19), (110, 19), (111, 19),
    (112, 19), (113, 19), (114, 19), (115, 19), (116, 19), (117, 19), (118, 19), (119, 19),
    (120, 19), (18, 18), (53, 16), (56, 11), (57, 8), (67, 7), (61, 3), (59, 2),
    (60, 1),
];

/// Coefficient table 0: code lengths and symbols, in code order (coef0_lens, coef0_syms).
const COEF0_LENS: [u8; 272] = [
    2, 9, 14, 14, 13, 12, 13, 14, 15, 15, 12, 10, 10, 10, 13, 14, 15, 15, 12, 11, 13, 14, 14, 13, 15, 15, 14, 12, 12,
    8, 10, 10, 15, 15, 14, 13, 14, 14, 13, 15, 20, 20, 19, 21, 21, 20, 19, 17, 17, 18, 18, 15, 15, 13, 12, 14, 15, 15,
    14, 15, 15, 12, 11, 6, 7, 8, 9, 13, 13, 13, 14, 14, 11, 10, 7, 8, 14, 14, 14, 14, 12, 13, 13, 12, 12, 12, 11, 9,
    13, 14, 14, 12, 11, 11, 11, 9, 8, 7, 14, 15, 15, 14, 14, 12, 13, 15, 16, 17, 17, 14, 12, 12, 12, 15, 15, 14, 14,
    14, 13, 13, 9, 9, 11, 11, 10, 7, 6, 13, 15, 15, 14, 14, 14, 13, 14, 15, 15, 13, 14, 14, 14, 14, 10, 9, 10, 10, 11,
    11, 10, 8, 9, 13, 14, 14, 12, 11, 14, 15, 15, 13, 12, 14, 14, 14, 14, 13, 14, 14, 3, 5, 8, 10, 10, 15, 15, 14, 14,
    16, 16, 15, 12, 11, 11, 11, 7, 8, 8, 9, 12, 13, 13, 12, 14, 15, 15, 13, 10, 11, 11, 13, 14, 14, 13, 14, 14, 11, 10,
    13, 15, 15, 14, 12, 11, 4, 6, 6, 8, 12, 12, 12, 13, 13, 12, 13, 13, 14, 14, 13, 13, 13, 9, 7, 9, 11, 14, 14, 13,
    14, 14, 13, 10, 8, 7, 5, 9, 12, 13, 14, 15, 15, 12, 12, 10, 14, 14, 13, 12, 13, 14, 14, 12, 13, 13, 12, 12, 12, 9,
    7, 6, 3, 4, 4,
];
const COEF0_SYMS: [u16; 272] = [
    2, 25, 111, 94, 69, 58, 87, 93, 136, 135, 59, 37, 34, 36, 82, 182, 120, 138, 195, 45, 168, 216, 178, 86, 140, 219,
    186, 162, 239, 18, 156, 35, 127, 236, 109, 85, 180, 253, 88, 147, 268, 264, 256, 266, 270, 262, 260, 248, 246, 252,
    258, 137, 189, 230, 64, 179, 146, 208, 101, 118, 238, 163, 46, 9, 153, 0, 26, 247, 169, 76, 202, 131, 194, 38, 13,
    19, 132, 106, 191, 97, 65, 198, 77, 62, 66, 164, 48, 27, 81, 183, 102, 60, 47, 49, 159, 227, 20, 14, 112, 263, 144,
    217, 104, 63, 79, 209, 269, 250, 254, 203, 241, 196, 61, 220, 148, 124, 185, 100, 80, 78, 193, 28, 50, 235, 41, 1,
    10, 171, 226, 150, 103, 114, 115, 170, 105, 211, 149, 249, 108, 188, 107, 255, 231, 155, 42, 40, 55, 160, 39, 21,
    29, 215, 234, 184, 228, 51, 116, 142, 145, 172, 165, 181, 130, 113, 117, 89, 128, 204, 3, 7, 154, 157, 43, 141,
    265, 133, 225, 271, 244, 221, 74, 54, 56, 52, 15, 222, 22, 30, 83, 199, 173, 73, 123, 210, 143, 175, 44, 53, 237,
    174, 139, 134, 110, 218, 129, 161, 213, 177, 267, 151, 125, 67, 223, 5, 11, 192, 23, 214, 243, 166, 200, 176, 68,
    224, 187, 257, 261, 232, 96, 251, 31, 16, 32, 57, 207, 121, 91, 126, 119, 99, 158, 24, 212, 8, 33, 70, 92, 205,
    240, 242, 75, 197, 233, 259, 190, 98, 71, 201, 122, 206, 72, 90, 95, 84, 167, 245, 229, 17, 12, 4, 152, 6,
];

/// Coefficient table 1: (symbol, code length), in code order (coef1_table).
#[rustfmt::skip]
const COEF1_TABLE: [Huff; 244] = [
    (2, 2), (3, 3), (102, 3), (4, 4), (148, 6), (134, 9), (171, 10), (18, 10),
    (11, 8), (159, 8), (14, 9), (156, 14), (235, 15), (61, 15), (38, 13), (153, 13),
    (48, 14), (49, 14), (23, 11), (203, 13), (208, 19), (204, 19), (129, 18), (94, 17),
    (87, 16), (62, 15), (174, 15), (147, 15), (29, 12), (191, 12), (64, 15), (65, 15),
    (146, 14), (164, 13), (142, 5), (132, 4), (103, 5), (154, 7), (165, 9), (181, 11),
    (109, 12), (30, 12), (86, 16), (92, 16), (239, 15), (138, 14), (39, 13), (50, 14),
    (115, 15), (238, 21), (228, 21), (236, 21), (222, 21), (216, 20), (226, 20), (196, 18),
    (192, 17), (120, 16), (221, 14), (51, 14), (24, 11), (143, 8), (7, 6), (9, 7),
    (152, 10), (136, 12), (160, 12), (241, 15), (66, 15), (168, 14), (219, 14), (113, 14),
    (193, 12), (19, 10), (173, 10), (105, 8), (149, 9), (15, 9), (205, 13), (207, 13),
    (125, 17), (190, 17), (182, 16), (68, 15), (70, 15), (67, 15), (137, 13), (31, 12),
    (223, 14), (116, 15), (210, 19), (220, 19), (198, 18), (126, 17), (88, 16), (41, 13),
    (25, 11), (40, 13), (73, 15), (243, 15), (53, 14), (195, 12), (183, 11), (225, 14),
    (52, 14), (71, 15), (121, 16), (89, 16), (170, 14), (55, 14), (69, 15), (83, 15),
    (209, 13), (108, 11), (32, 12), (54, 14), (122, 16), (184, 16), (176, 15), (42, 13),
    (12, 8), (161, 8), (6, 5), (167, 9), (106, 9), (20, 10), (145, 12), (111, 13),
    (43, 13), (26, 11), (175, 10), (107, 10), (34, 12), (33, 12), (197, 12), (74, 15),
    (128, 17), (232, 20), (212, 20), (224, 19), (202, 18), (90, 16), (57, 14), (227, 14),
    (97, 16), (93, 16), (140, 15), (185, 11), (27, 11), (16, 9), (158, 11), (211, 13),
    (56, 14), (117, 15), (72, 15), (166, 13), (91, 16), (95, 16), (80, 15), (101, 16),
    (194, 17), (127, 17), (82, 15), (21, 10), (144, 10), (177, 10), (151, 6), (10, 7),
    (157, 7), (8, 6), (5, 4), (13, 8), (0, 9), (213, 13), (46, 13), (199, 12),
    (35, 12), (162, 12), (135, 10), (169, 9), (45, 13), (59, 14), (114, 14), (44, 13),
    (188, 16), (186, 16), (75, 15), (79, 15), (118, 15), (187, 11), (112, 13), (139, 14),
    (178, 15), (81, 15), (110, 12), (28, 11), (163, 8), (133, 6), (104, 6), (17, 9),
    (22, 10), (229, 14), (172, 14), (217, 13), (201, 12), (36, 12), (218, 20), (242, 22),
    (240, 22), (234, 21), (230, 19), (206, 18), (200, 18), (214, 18), (130, 17), (131, 17),
    (141, 15), (84, 15), (76, 15), (215, 13), (58, 14), (231, 14), (233, 14), (180, 15),
    (77, 15), (37, 12), (189, 11), (179, 10), (155, 10), (47, 13), (96, 16), (99, 16),
    (119, 15), (63, 14), (237, 14), (78, 15), (85, 15), (60, 14), (98, 16), (100, 16),
    (124, 16), (123, 16), (150, 11), (1, 7),
];

/// Each symbol's run and level (coef0_run, coef0_level, coef1_run, coef1_level).
const COEF0_RUN: [u8; 272] = [
    0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29,
    30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58,
    59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87,
    88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112,
    113, 114, 115, 116, 117, 118, 119, 120, 121, 122, 123, 124, 125, 126, 127, 128, 129, 130, 131, 132, 133, 134, 135,
    136, 137, 138, 139, 140, 141, 142, 143, 144, 145, 146, 147, 148, 149, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13,
    14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 0, 1, 2, 3,
    4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0, 1, 2, 3, 4, 0, 1, 0, 1,
    0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1,
    0, 1, 0,
];
const COEF0_LEVEL: [u8; 272] = [
    0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 5, 5, 5, 5, 5, 6,
    6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14, 15, 15, 16, 16, 17, 17, 18, 18, 19, 19, 20, 20, 21,
    21, 22, 22, 23, 23, 24, 24, 25, 25, 26, 26, 27, 27, 28,
];
const COEF1_RUN: [u8; 244] = [
    0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29,
    30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58,
    59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87,
    88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
    19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0, 1, 2, 3, 4, 5, 0, 1, 2, 0, 1, 2, 0, 1,
    2, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0,
    1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0,
    1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 0, 0,
];
const COEF1_LEVEL: [u8; 244] = [
    0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 5, 5, 5, 6,
    6, 6, 7, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14, 15, 15, 16, 16, 17, 17, 18, 18, 19, 19, 20, 20,
    21, 21, 22, 22, 23, 23, 24, 24, 25, 25, 26, 26, 27, 27, 28, 28, 29, 29, 30, 30, 31, 31, 32, 32, 33, 33, 34, 34, 35,
    35, 36, 36, 37, 37, 38, 38, 39, 39, 40, 40, 41, 41, 42, 42, 43, 43, 44, 44, 45, 45, 46, 46, 47, 47, 48, 48, 49, 49,
    50, 51, 52,
];

// ---- bits -------------------------------------------------------------------------------

/// Bits written most significant first.
#[derive(Default)]
struct Bits {
    bytes: Vec<u8>,
    size: usize,
}

impl Bits {
    fn put(&mut self, v: u64, n: u32) {
        for i in (0..n).rev() {
            self.put_bit((v >> i) & 1 != 0);
        }
    }

    fn append(&mut self, b: &Bits) {
        for i in 0..b.size {
            self.put_bit(b.bit(i));
        }
    }

    fn bit(&self, i: usize) -> bool {
        (self.bytes[i / 8] >> (7 - i % 8)) & 1 != 0
    }

    /// Zeros up to a whole number of `n` bits.
    fn pad_to(&mut self, n: usize) {
        while !self.size.is_multiple_of(n) {
            self.put_bit(false);
        }
    }

    fn put_bit(&mut self, b: bool) {
        if self.size.is_multiple_of(8) {
            self.bytes.push(0);
        }
        if b && let Some(last) = self.bytes.last_mut() {
            *last |= 0x80 >> (self.size % 8);
        }
        self.size += 1;
    }

    fn put_code(&mut self, c: Code) {
        self.put(u64::from(c.bits), c.length);
    }
}

#[derive(Clone, Copy, Default)]
struct Code {
    bits: u32,
    length: u32,
}

/// Codes for the symbols of a table of (symbol, length) in code order, as FFmpeg's
/// ff_vlc_init_from_lengths hands them out.
fn canonical(table: impl IntoIterator<Item = Huff>) -> Vec<Code> {
    let mut codes = Vec::new();
    let mut code: u64 = 0;
    for (symbol, length) in table {
        if length > 0 {
            let at = usize::from(symbol);
            if codes.len() <= at {
                codes.resize(at + 1, Code::default());
            }
            let length = u32::from(length);
            codes[at] = Code { bits: (code >> (32 - length)) as u32, length };
            code += 1 << (32 - length);
        }
    }
    codes
}

/// A coefficient table: each symbol's code, and the symbol of a run of zeros and the level
/// after it (2 and up; 0 escapes, 1 ends a block).
struct CoefTable {
    codes: Vec<Code>,
    run_level: BTreeMap<(i32, i32), usize>,
}

impl CoefTable {
    fn new(table: impl IntoIterator<Item = Huff>, runs: &[u8], levels: &[u8]) -> CoefTable {
        let mut run_level = BTreeMap::new();
        for (sym, (&run, &level)) in runs.iter().zip(levels).enumerate().skip(2) {
            run_level.entry((i32::from(run), i32::from(level))).or_insert(sym);
        }
        CoefTable { codes: canonical(table), run_level }
    }

    fn code(&self, sym: usize) -> Code {
        // Every symbol of the run and level tables has a code.
        self.codes.get(sym).copied().unwrap_or_default()
    }
}

/// The scale factor codes, by difference + 60.
fn scale_codes() -> &'static [Code] {
    static CODES: OnceLock<Vec<Code>> = OnceLock::new();
    CODES.get_or_init(|| canonical(SCALE_TABLE))
}

fn coefs(t: usize) -> &'static CoefTable {
    static TABLES: OnceLock<[CoefTable; 2]> = OnceLock::new();
    let tables = TABLES.get_or_init(|| {
        [
            CoefTable::new(COEF0_SYMS.into_iter().zip(COEF0_LENS), &COEF0_RUN, &COEF0_LEVEL),
            CoefTable::new(COEF1_TABLE, &COEF1_RUN, &COEF1_LEVEL),
        ]
    });
    &tables[t & 1]
}

// ---- a frame ----------------------------------------------------------------------------

/// Scale factor band edges of a 512-sample subframe, as the decoder works them out.
fn band_offsets(rate: u32) -> Vec<usize> {
    let r = if rate > 44100 {
        48000
    } else if rate > 32000 {
        44100
    } else if rate > 24000 {
        32000
    } else {
        24000
    };
    let mut offs = vec![0];
    for f in CRITICAL_FREQ {
        let o = (((FRAME as i32 * 2 * f) / r + 2) & !3) as usize;
        if offs.last().is_some_and(|&last| o > last) {
            offs.push(o);
        }
        if o >= FRAME {
            break;
        }
    }
    if let Some(last) = offs.last_mut() {
        *last = FRAME;
    }
    offs
}

/// A sum added up pairwise, eight at a time: the order a band's level comes out of decides
/// its last bits.
fn pairwise_sum(a: &[f64]) -> f64 {
    let n = a.len();
    if n < 8 {
        let mut s = 0.0;
        for &x in a {
            s += x;
        }
        return s;
    }
    if n <= 128 {
        let mut r = [0.0; 8];
        r.copy_from_slice(&a[..8]);
        let mut i = 8;
        while i < n - n % 8 {
            for (j, r) in r.iter_mut().enumerate() {
                *r += a[i + j];
            }
            i += 8;
        }
        let mut s = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        for &x in &a[i..] {
            s += x;
        }
        return s;
    }
    let mut half = n / 2;
    half -= half % 8;
    pairwise_sum(&a[..half]) + pairwise_sum(&a[half..])
}

/// A value after the prefix code saying how many bits it takes.
fn prefixed(b: &mut Bits, prefix: u64, prefix_bits: u32, v: u64, bits: u32) {
    b.put(prefix, prefix_bits);
    b.put(v, bits);
}

fn large(b: &mut Bits, v: u32) {
    let v = u64::from(v);
    if v < 1 << 8 {
        prefixed(b, 0, 1, v, 8);
    } else if v < 1 << 16 {
        prefixed(b, 2, 2, v, 16);
    } else if v < 1 << 24 {
        prefixed(b, 6, 3, v, 24);
    } else {
        prefixed(b, 7, 3, v, 31);
    }
}

/// Run-level coding of a frame's whole-number coefficients with coefficient table t.
fn coefficients(q: &[i64], t: usize) -> Bits {
    let table = coefs(t);
    let mut b = Bits::default();
    let mut prev: i64 = -1;
    for (i, &v) in q.iter().enumerate() {
        if v == 0 {
            continue;
        }
        let i = i as i64;
        let run = i - prev - 1;
        let level = v.wrapping_abs();
        if let Some(&sym) = table.run_level.get(&(run as i32, level as i32)) {
            b.put_code(table.code(sym));
        } else {
            b.put_code(table.code(0)); // escape: the level, then the run
            large(&mut b, level as u32);
            if run == 0 {
                b.put(0, 1);
            } else if run <= 4 {
                prefixed(&mut b, 2, 2, (run - 1) as u64, 2);
            } else {
                prefixed(&mut b, 6, 3, (run - 4) as u64, 9);
            }
        }
        b.put(u64::from(v > 0), 1);
        prev = i;
    }
    if prev + 1 < q.len() as i64 {
        b.put_code(table.code(1)); // end of block
    }
    b
}

/// One frame's bits (after its length, before its two end bits) for its 512 coefficients.
fn frame(x: &[f64; FRAME], offs: &[usize], first: bool) -> Bits {
    let mask = 10.0f64.powf(-MASK_DB / 20.0);
    let levels: Vec<f64> = offs
        .windows(2)
        .map(|w| {
            let band = &x[w[0]..w[1]];
            let squares: Vec<f64> = band.iter().map(|&v| v * v).collect();
            (pairwise_sum(&squares) / band.len() as f64).sqrt()
        })
        .collect();
    let top = levels.iter().copied().fold(f64::NEG_INFINITY, |m, l| if m < l { l } else { m });
    let mut d: Vec<i32> = levels
        .iter()
        .map(|&level| {
            let loudest = [level / QUALITY, top * mask, FLOOR]
                .into_iter()
                .reduce(|m, v| if m < v { v } else { m })
                .unwrap_or(FLOOR);
            (20.0 * loudest.log10()).round_ties_even() as i32
        })
        .collect();
    for i in 1..d.len() {
        // a scale factor may move by at most 60 a band
        let p = d[i - 1];
        d[i] = d[i].clamp(p - 60, p + 60);
    }
    let big_q = d.iter().copied().max().unwrap_or(0);
    let mut q = vec![0i64; FRAME];
    let mut any = false;
    for (w, &di) in offs.windows(2).zip(&d) {
        let step = 10.0f64.powf(f64::from(di) / 20.0);
        for k in w[0]..w[1] {
            q[k] = truncate((x[k] / step).round_ties_even());
            any |= q[k] != 0;
        }
    }
    let mut f = Bits::default();
    f.put(1, 1); // all channels share the subframe layout
    f.put(0, 1); // one 512-sample subframe
    f.put(0, 8); // dynamic range gain
    if first {
        // Trim 512 samples at the start, none at the end.
        f.put(1, 1);
        f.put(1, 1);
        f.put(512, 10);
        f.put(0, 1);
    } else {
        f.put(0, 1); // no trims
    }
    f.put(0, 1); // no extended subframe header
    f.put(0, 1); // reserved
    if !any {
        f.put(0, 1); // no coefficients
        return f;
    }
    f.put(1, 1);
    f.put(1, 1); // 0 vector coded coefficients: all run-level coded
    f.put(0, 8);
    let step = big_q - 90;
    if (-31..=30).contains(&step) {
        f.put(step as u64, 6);
    } else {
        let mut rest = if step < 0 { 58 - big_q } else { big_q - 121 };
        f.put(if step < 0 { -32i64 as u64 } else { 31 }, 6);
        while rest >= 31 {
            f.put(31, 5);
            rest -= 31;
        }
        f.put(rest as u64, 5);
    }
    f.put(0, 2); // scale factor step 1 (dB)
    let mut val = 45;
    for &v in &d {
        let s = v - big_q + 60;
        // The clamp above keeps every difference within the table.
        let code = usize::try_from(s - val + 60).ok().and_then(|i| scale_codes().get(i));
        f.put_code(code.copied().unwrap_or_default());
        val = s;
    }
    let c0 = coefficients(&q, 0);
    let c1 = coefficients(&q, 1);
    let use0 = c0.size <= c1.size;
    f.put(u64::from(!use0), 1);
    f.append(if use0 { &c0 } else { &c1 });
    f
}

/// The transform: a frame's 512 coefficients are its 1,024 samples, windowed, times this
/// matrix. Each coefficient adds its products up in sample order; kept a sample's column
/// at a time, all 512 sums go forward together.
struct Transform {
    window: Vec<f64>,
    /// 1,024 columns of 512.
    mdct: Vec<f64>,
}

impl Transform {
    fn new() -> Transform {
        let pi = std::f64::consts::PI;
        let window = (0..WINDOW).map(|n| (pi * (n as f64 + 0.5) / WINDOW as f64).sin()).collect();
        let mut mdct = Vec::with_capacity(FRAME * WINDOW);
        for n in 0..WINDOW {
            for k in 0..FRAME {
                let angle = pi / FRAME as f64 * (n as f64 + 0.5 + (FRAME / 2) as f64) * (k as f64 + 0.5);
                mdct.push(angle.cos() * GAIN);
            }
        }
        Transform { window, mdct }
    }

    fn apply(&self, samples: &[f64], out: &mut [f64; FRAME]) {
        out.fill(0.0);
        for ((&s, &win), column) in samples.iter().zip(&self.window).zip(self.mdct.chunks_exact(FRAME)) {
            let w = s * win;
            for (o, &c) in out.iter_mut().zip(column) {
                *o += w * c;
            }
        }
    }
}

/// Frames to packets: each frame is its 15-bit length, its bits, a 1 and a trailer bit (1
/// when the next frame ends in the same packet too).
fn pack(frames: &[Bits], samples: u32, rate: u32) -> Xma {
    let mut starts = Vec::with_capacity(frames.len());
    let mut pos: u64 = 0;
    for f in frames {
        starts.push(pos);
        pos += 15 + f.size as u64 + 2;
    }
    let end = |i: usize| starts.get(i + 1).copied().unwrap_or(pos);
    let mut stream = Bits::default();
    for (i, f) in frames.iter().enumerate() {
        let more = i + 1 < frames.len() && (end(i) - 1) / PAYLOAD_BITS == (end(i + 1) - 1) / PAYLOAD_BITS;
        stream.put(15 + f.size as u64 + 2, 15);
        stream.append(f);
        stream.put(1, 1);
        stream.put(u64::from(more), 1);
    }
    stream.pad_to(PAYLOAD_BITS as usize);
    let mut out = Xma { samples, rate, ..Xma::default() };
    let packets = stream.size as u64 / PAYLOAD_BITS;
    let mut k = 0;
    for p in 0..packets {
        let a = p * PAYLOAD_BITS;
        while k < starts.len() && starts[k] < a {
            k += 1;
        }
        out.seek.push((FRAME * k) as u32);
        // The bits of the frame carried over from the packet before.
        let prev = match starts.get(k) {
            Some(&start) if start < a + PAYLOAD_BITS => start - a,
            _ => PAYLOAD_BITS,
        };
        let header = (((p & 15) as u32) << 28) | (2 << 26) | ((prev.min(0x7FFF) << 11) as u32);
        out.data.extend_from_slice(&header.to_be_bytes());
        let from = (a / 8) as usize;
        out.data.extend_from_slice(&stream.bytes[from..from + (PAYLOAD_BITS / 8) as usize]);
    }
    out.end_bit = pos + HEADER_BITS * ((pos - 1) / PAYLOAD_BITS + 1);
    out
}

/// Encodes interleaved 16-bit samples of one or two channels (stereo is mixed to mono, the
/// only layout written).
pub fn encode_xma(pcm: &[i16], rate: u32, channels: i32) -> Xma {
    let stride = channels as usize;
    let n = pcm.len().checked_div(stride).unwrap_or(0);
    let frames = (LEAD + n).div_ceil(FRAME) + 1;
    let mut padded = vec![0.0f64; frames * FRAME + FRAME];
    for (i, frame) in pcm.chunks_exact(stride.max(1)).take(n).enumerate() {
        let mut s = 0.0;
        for &v in frame {
            s += f64::from(v) / 32768.0;
        }
        padded[LEAD + i] = if channels > 1 { s / f64::from(channels) } else { s };
    }
    static TRANSFORM: OnceLock<Transform> = OnceLock::new();
    let transform = TRANSFORM.get_or_init(Transform::new);
    let offs = band_offsets(rate);
    let mut x = [0.0; FRAME];
    let bits: Vec<Bits> = (0..frames)
        .map(|i| {
            transform.apply(&padded[i * FRAME..], &mut x);
            frame(&x, &offs, i == 0)
        })
        .collect();
    pack(&bits, n as u32, rate)
}

/// 16-bit PCM: interleaved samples, their rate and channel count.
#[derive(Clone, Debug, Default)]
pub struct Pcm {
    pub samples: Vec<i16>,
    pub rate: u32,
    pub channels: i32,
}

/// What a PC sound file holds for the encoder.
pub struct SoundPcm {
    pub pcm: Option<Pcm>,
    /// Another format (ADPCM, 8-bit, MP3, ...), which isn't decoded.
    pub compressed: bool,
}

#[derive(Clone, Copy)]
pub struct PcmFormat {
    pub rate: u32,
    pub channels: i32,
}

/// `size` little-endian bytes at `at`, 0 past the end.
fn le(b: &[u8], at: usize, size: usize) -> u32 {
    b.get(at..at + size).map_or(0, |p| p.iter().rev().fold(0, |v, &x| (v << 8) | u32::from(x)))
}

/// Whole frames of 16-bit samples.
fn samples(data: &[u8], rate: u32, channels: i32) -> Pcm {
    let frame = (channels as usize).wrapping_mul(2);
    let whole = data.len().checked_div(frame).map_or(0, |f| f * frame);
    let samples = data[..whole].chunks_exact(2).map(|p| i16::from_le_bytes([p[0], p[1]])).collect();
    Pcm { samples, rate, channels }
}

/// The audio of a .wav file (RIFF, mono or stereo), or else of plain 16-bit samples in the
/// format given (a PC LoadedSound's).
pub fn read_sound(raw: &[u8], plain: Option<PcmFormat>) -> SoundPcm {
    let is = |at: usize, tag: &[u8; 4]| raw.get(at..at + 4) == Some(tag);
    if is(0, b"RIFF") && is(8, b"WAVE") {
        let (mut fmt, mut data) = (None, None);
        let mut i = 12;
        while i + 8 <= raw.len() {
            let length = le(raw, i + 4, 4) as usize;
            let body = &raw[i + 8..i + 8 + length.min(raw.len() - (i + 8))];
            if is(i, b"fmt ") {
                fmt = Some(body);
            } else if is(i, b"data") {
                data = Some(body);
            }
            i += 8 + length + (length & 1);
        }
        let (Some(fmt), Some(data)) = (fmt, data) else {
            return SoundPcm { pcm: None, compressed: false };
        };
        if fmt.len() < 16 {
            return SoundPcm { pcm: None, compressed: false };
        }
        let mut tag = le(fmt, 0, 2);
        let channels = le(fmt, 2, 2) as i32;
        let rate = le(fmt, 4, 4);
        let bits = le(fmt, 14, 2);
        if tag == 0xFFFE && fmt.len() >= 26 {
            tag = le(fmt, 24, 2); // WAVE_FORMAT_EXTENSIBLE: its subformat
        }
        if tag != 1 || bits != 16 || (channels != 1 && channels != 2) || rate == 0 || data.len() < 2 * channels as usize
        {
            return SoundPcm { pcm: None, compressed: true };
        }
        return SoundPcm { pcm: Some(samples(data, rate, channels)), compressed: false };
    }
    match plain {
        Some(p) => SoundPcm { pcm: Some(samples(raw, p.rate, p.channels)), compressed: false },
        None => SoundPcm { pcm: None, compressed: true },
    }
}
