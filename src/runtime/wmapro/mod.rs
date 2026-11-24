// The XMA frame decoder: FFmpeg's WMA Pro decoder (libavcodec/wmaprodec.c) as its
// "xmaframes" codec drives it, one XMA frame at a time, in Rust. It follows the C step by
// step -- the frame's tiling into subframes, channel transforms, vector and run-level coded
// coefficients, scale factors, the inverse MDCT and the sine-windowed overlap -- so that
// it gives the same floats bit for bit, logs what FFmpeg logged, and fails where it failed.
//
// XMA is WMA Pro with fixed settings: 512 samples a frame in up to four subframes of 512,
// 256 or 128, 16-bit scaling, a 15-bit length before each frame, dynamic range compression
// data, and one or two channels.
//
// Its code tables are complete, so no code is invalid and nothing reads outside them.
//
// Copyright (c) 2007 Baptiste Coudurier, Benjamin Larsson, Ulion
// Copyright (c) 2008 - 2009 Sascha Sommer, Benjamin Larsson
// Copyright (c) 2002-2007 The FFmpeg Project (wma.c)
// Copyright 2005 Balatoni Denes, Copyright 2006 Loren Merritt (float_dsp.c)
// Copyright (c) 2009 Reimar Döffinger (sinewin_tablegen.h)
//
// This file is part of a port of FFmpeg and, like it, is free software; you can
// redistribute it and/or modify it under the terms of the GNU Lesser General Public License
// as published by the Free Software Foundation; either version 2.1 of the License, or (at
// your option) any later version. It is distributed in the hope that it will be useful, but
// WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR
// A PARTICULAR PURPOSE. See the GNU Lesser General Public License for more details
// (LICENSE.LGPL-2.1 in this folder, and NOTICE.md).

mod bits;
mod mdct;
mod tables;
mod vlc;

use bits::BitReader;
use mdct::Mdct;
use std::sync::LazyLock;
use tables::*;
use vlc::VlcElem;

pub const SAMPLES: usize = 512;

const MAX_CHANNELS: usize = 8;
const MAX_SUBFRAMES: usize = 32;
const MAX_BANDS: usize = 29;
const MAX_FRAMESIZE: usize = 32768;
const PADDING: usize = 64; // AV_INPUT_BUFFER_PADDING_SIZE
const BLOCK_MIN_BITS: u32 = 6;
const BLOCK_MAX_SIZE: usize = 1 << 13;
const BLOCK_SIZES: usize = 8;
const OUT_SIZE: usize = BLOCK_MAX_SIZE + BLOCK_MAX_SIZE / 2;

const VLCBITS: u32 = 9;
const SCALEVLCBITS: u32 = 8;
// The lookups a code of the longest length takes.
const VEC4MAXDEPTH: u32 = (HUFF_VEC4_MAXBITS as u32).div_ceil(VLCBITS);
const VEC2MAXDEPTH: u32 = (HUFF_VEC2_MAXBITS as u32).div_ceil(VLCBITS);
const VEC1MAXDEPTH: u32 = (HUFF_VEC1_MAXBITS as u32).div_ceil(VLCBITS);
const SCALEMAXDEPTH: u32 = (HUFF_SCALE_MAXBITS as u32).div_ceil(SCALEVLCBITS);
const SCALERLMAXDEPTH: u32 = (HUFF_SCALE_RL_MAXBITS as u32).div_ceil(VLCBITS);
const VLCMAX: u32 = 22u32.div_ceil(VLCBITS); // wma.h's, for the run-level codes

// The settings XMA fixes (decode_init for AV_CODEC_ID_XMAFRAMES).
const DECODE_FLAGS: u32 = 0x10d6;
const BITS_PER_SAMPLE: u32 = 16;
const LOG2_FRAME_SIZE: u32 = 11 + 4; // av_log2(block_align 2048) + 4

/// av_log2: the index of the highest bit set, 0 for 0.
fn av_log2(value: u32) -> u32 {
    31 - (value | 1).leading_zeros()
}

// What decode_init_static makes once: the VLC tables, the decorrelation sines and the sine
// windows for every block size.
struct Statics {
    sf: Vec<VlcElem>,
    sf_rl: Vec<VlcElem>,
    coef: [Vec<VlcElem>; 2],
    vec4: Vec<VlcElem>,
    vec2: Vec<VlcElem>,
    vec1: Vec<VlcElem>,
    sin64: [f32; 33],
    windows: Vec<Vec<f32>>, // by log2 of the window length, 6 to 13
}

fn lengths_and_symbols(pairs: &[[u8; 2]]) -> (Vec<i8>, Vec<u16>) {
    (pairs.iter().map(|pair| pair[1] as i8).collect(), pairs.iter().map(|pair| u16::from(pair[0])).collect())
}

static STATICS: LazyLock<Statics> = LazyLock::new(|| {
    let (sf_lens, sf_syms) = lengths_and_symbols(&SCALE_TABLE);
    let (sf_rl_lens, sf_rl_syms) = lengths_and_symbols(&SCALE_RL_TABLE);
    let (coef1_lens, coef1_syms) = lengths_and_symbols(&COEF1_TABLE);
    let (vec2_lens, vec2_syms) = lengths_and_symbols(&VEC2_TABLE);
    let (vec1_lens, vec1_syms) = lengths_and_symbols(&VEC1_TABLE);
    let coef0_lens: Vec<i8> = COEF0_LENS.iter().map(|&l| l as i8).collect();
    let vec4_lens: Vec<i8> = VEC4_LENS.iter().map(|&l| l as i8).collect();
    Statics {
        sf: vlc::from_lengths(SCALEVLCBITS as i32, &sf_lens, &sf_syms, -60),
        sf_rl: vlc::from_lengths(VLCBITS as i32, &sf_rl_lens, &sf_rl_syms, 0),
        coef: [
            vlc::from_lengths(VLCBITS as i32, &coef0_lens, &COEF0_SYMS, 0),
            vlc::from_lengths(VLCBITS as i32, &coef1_lens, &coef1_syms, 0),
        ],
        vec4: vlc::from_lengths(VLCBITS as i32, &vec4_lens, &VEC4_SYMS, -1),
        vec2: vlc::from_lengths(VLCBITS as i32, &vec2_lens, &vec2_syms, -1),
        vec1: vlc::from_lengths(VLCBITS as i32, &vec1_lens, &vec1_syms, 0),
        sin64: std::array::from_fn(|i| (i as f64 * std::f64::consts::PI / 64.0).sin() as f32),
        // ff_sine_window_init: the angle as a double, sinf of it as a float.
        windows: (0..=13u32)
            .map(|log2| {
                if log2 < BLOCK_MIN_BITS {
                    return Vec::new();
                }
                let n = 1usize << log2;
                (0..n).map(|i| (((i as f64 + 0.5) * (std::f64::consts::PI / (2.0 * n as f64))) as f32).sin()).collect()
            })
            .collect(),
    }
});

// The inverse MDCTs, one per block size the decoder can meet (128, 256 and 512, and 64,
// which FFmpeg makes too), each with the scale for 16-bit samples.
struct Transforms {
    mdcts: Vec<Mdct>,
}

fn make_transforms() -> Transforms {
    Transforms {
        mdcts: (0..4)
            .map(|i| {
                let scale =
                    (1.0 / f64::from(1u32 << (BLOCK_MIN_BITS + i - 1)) / (1i64 << (BITS_PER_SAMPLE - 1)) as f64) as f32;
                Mdct::new(1 << (BLOCK_MIN_BITS + i), scale)
            })
            .collect(),
    }
}

#[derive(Clone, Copy)]
struct Channel {
    prev_block_len: i16,
    transmit_coefs: u8,
    num_subframes: u8,
    subframe_len: [u16; MAX_SUBFRAMES],
    subframe_offset: [u16; MAX_SUBFRAMES],
    cur_subframe: u8,
    decoded_samples: u16,
    grouped: u8,
    quant_step: i32,
    reuse_sf: i8,
    scale_factor_step: i8,
    max_scale_factor: i32,
    saved_scale_factors: [[i32; MAX_BANDS]; 2],
    scale_factor_idx: i8,
    scale_factors: usize, // which of saved_scale_factors
    table_idx: u8,
    coeffs: usize, // where in `out`
    num_vec_coeffs: u16,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            prev_block_len: 0,
            transmit_coefs: 0,
            num_subframes: 0,
            subframe_len: [0; MAX_SUBFRAMES],
            subframe_offset: [0; MAX_SUBFRAMES],
            cur_subframe: 0,
            decoded_samples: 0,
            grouped: 0,
            quant_step: 0,
            reuse_sf: 0,
            scale_factor_step: 0,
            max_scale_factor: 0,
            saved_scale_factors: [[0; MAX_BANDS]; 2],
            scale_factor_idx: 0,
            scale_factors: 0,
            table_idx: 0,
            coeffs: 0,
            num_vec_coeffs: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct ChannelGroup {
    num_channels: u8,
    transform: i8,
    transform_band: [i8; MAX_BANDS],
    decorrelation_matrix: [f32; MAX_CHANNELS * MAX_CHANNELS],
    channel_data: [usize; MAX_CHANNELS], // the channels whose coefficients these are
}

impl Default for ChannelGroup {
    fn default() -> Self {
        Self {
            num_channels: 0,
            transform: 0,
            transform_band: [0; MAX_BANDS],
            decorrelation_matrix: [0.0; MAX_CHANNELS * MAX_CHANNELS],
            channel_data: [0; MAX_CHANNELS],
        }
    }
}

/// What a frame came to, as FFmpeg's send and receive calls told it.
pub enum Decoded {
    /// The channels' samples are in the output.
    Frame,
    /// No frame came out (a damaged frame: FFmpeg's EAGAIN).
    None,
    /// The packet was refused, with FFmpeg's text for the error.
    Refused(&'static std::ffi::CStr),
}

const INVALID_DATA: &std::ffi::CStr = c"Invalid data found when processing input";

// A decode step's failure; the frame is given up.
struct Failed;

/// One context's decoder, at one rate and channel count.
pub struct Decoder {
    nb_channels: usize,
    sample_rate: i32,
    // decode_init's
    samples_per_frame: i32,
    max_num_subframes: i32,
    max_subframe_len_bit: bool,
    subframe_len_bits: u32,
    min_samples_per_subframe: i32,
    dynamic_range_compression: bool,
    len_prefix: bool,
    num_sfb: [i8; BLOCK_SIZES],
    sfb_offsets: [[i16; MAX_BANDS]; BLOCK_SIZES],
    sf_offsets: [[[i8; MAX_BANDS]; BLOCK_SIZES]; BLOCK_SIZES],
    transforms: Transforms,
    // the frame
    frame_data: Vec<u8>, // MAX_FRAMESIZE + PADDING, kept from frame to frame
    num_saved_bits: i32,
    frame_offset: i32,
    frame_num: u32,
    drc_gain: u8,
    trim_start: u16,
    trim_end: u16,
    parsed_all_subframes: bool,
    // the subframe
    subframe_len: i16,
    channels_for_cur_subframe: usize,
    channel_indexes_for_cur_subframe: [usize; MAX_CHANNELS],
    num_bands: usize,
    transmit_num_vec_coeffs: bool,
    table_idx: usize,
    esc_len: u32,
    num_chgroups: usize,
    chgroup: [ChannelGroup; MAX_CHANNELS],
    channel: [Channel; MAX_CHANNELS],
    out: Vec<[f32; OUT_SIZE]>,
    tmp: Box<[f32; BLOCK_MAX_SIZE]>,
}

/// Where FFmpeg's log goes: one line a call, without its newline.
pub type Log<'a> = &'a mut dyn FnMut(&str);

// avpriv_request_sample: three warnings.
fn request_sample(log: Log, what: &str) {
    log(what);
    log(
        " is not implemented. Update your FFmpeg version to the newest one from Git. If the problem still occurs, it means that your file has a feature which has not been implemented.",
    );
    log(
        "If you want to help, upload a sample of this file to https://streams.videolan.org/upload/ and contact the ffmpeg-devel mailing list. (ffmpeg-devel@ffmpeg.org)",
    );
}

// get_rate, for XMA.
fn rate_class(sample_rate: i32) -> i32 {
    if sample_rate > 44100 {
        48000
    } else if sample_rate > 32000 {
        44100
    } else if sample_rate > 24000 {
        32000
    } else {
        24000
    }
}

impl Decoder {
    /// decode_init for xmaframes; `channels` is 1 or 2.
    pub fn new(sample_rate: i32, channels: usize) -> Box<Self> {
        LazyLock::force(&STATICS);
        let samples_per_frame = SAMPLES as i32;
        let log2_max_num_subframes = (DECODE_FLAGS & 0x38) >> 3;
        let max_num_subframes = 1i32 << log2_max_num_subframes;
        let num_possible_block_sizes = log2_max_num_subframes as usize + 1;
        let mut decoder = Box::new(Self {
            nb_channels: channels,
            sample_rate,
            samples_per_frame,
            max_num_subframes,
            max_subframe_len_bit: max_num_subframes == 16 || max_num_subframes == 4,
            subframe_len_bits: av_log2(log2_max_num_subframes) + 1,
            min_samples_per_subframe: samples_per_frame / max_num_subframes,
            dynamic_range_compression: DECODE_FLAGS & 0x80 != 0,
            len_prefix: DECODE_FLAGS & 0x40 != 0,
            num_sfb: [0; BLOCK_SIZES],
            sfb_offsets: [[0; MAX_BANDS]; BLOCK_SIZES],
            sf_offsets: [[[0; MAX_BANDS]; BLOCK_SIZES]; BLOCK_SIZES],
            transforms: make_transforms(),
            frame_data: vec![0; MAX_FRAMESIZE + PADDING],
            num_saved_bits: 0,
            frame_offset: 0,
            frame_num: 0,
            drc_gain: 0,
            trim_start: 0,
            trim_end: 0,
            parsed_all_subframes: false,
            subframe_len: 0,
            channels_for_cur_subframe: 0,
            channel_indexes_for_cur_subframe: [0; MAX_CHANNELS],
            num_bands: 0,
            transmit_num_vec_coeffs: false,
            table_idx: 0,
            esc_len: 0,
            num_chgroups: 0,
            chgroup: [ChannelGroup::default(); MAX_CHANNELS],
            channel: [Channel::default(); MAX_CHANNELS],
            out: vec![[0.0; OUT_SIZE]; channels],
            tmp: Box::new([0.0; BLOCK_MAX_SIZE]),
        });
        let d = &mut *decoder;
        for c in 0..channels {
            d.channel[c].prev_block_len = samples_per_frame as i16;
        }

        // The scale factor bands of each block size.
        let rate = rate_class(sample_rate);
        for i in 0..num_possible_block_sizes {
            let subframe_len = samples_per_frame >> i;
            let mut band = 1;
            d.sfb_offsets[i][0] = 0;
            let mut x = 0;
            while x < MAX_BANDS - 1 && i32::from(d.sfb_offsets[i][band - 1]) < subframe_len {
                let mut offset = (subframe_len * 2 * i32::from(CRITICAL_FREQ[x])) / rate + 2;
                offset &= !3;
                if offset > i32::from(d.sfb_offsets[i][band - 1]) {
                    d.sfb_offsets[i][band] = offset as i16;
                    band += 1;
                }
                if offset >= subframe_len {
                    break;
                }
                x += 1;
            }
            d.sfb_offsets[i][band - 1] = subframe_len as i16;
            d.num_sfb[i] = (band - 1) as i8;
        }

        // Which band of another block size a band's scale factor comes from.
        for i in 0..num_possible_block_sizes {
            for b in 0..d.num_sfb[i] as usize {
                let offset = ((i32::from(d.sfb_offsets[i][b]) + i32::from(d.sfb_offsets[i][b + 1]) - 1) << i) >> 1;
                for x in 0..num_possible_block_sizes {
                    let mut v = 0;
                    while (i32::from(d.sfb_offsets[x][v + 1]) << x) < offset {
                        v += 1;
                        assert!(v < MAX_BANDS);
                    }
                    d.sf_offsets[i][x][b] = v as i8;
                }
            }
        }
        decoder
    }

    pub fn sample_rate(&self) -> i32 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.nb_channels
    }

    /// One XMA frame, after a byte giving its padding: `data` holds `size` bytes and at
    /// least 64 more. The channels' samples go to `output` when a frame comes out.
    pub fn decode(&mut self, data: &[u8], size: usize, output: &mut [[f32; SAMPLES]; 2], log: Log) -> Decoded {
        // xmaframes_decode_packet
        if size < 3 {
            log(&format!("XMA frame too small, {size} bytes"));
            return Decoded::Refused(INVALID_DATA);
        }
        let buf_bit_size = (size << 3) as i32;
        let mut gb = BitReader::new(data, buf_bit_size);
        let padding_start = gb.get(3);
        let padding_end = gb.get(3);
        gb.skip(2);
        gb.skip(padding_start);
        let xma_frame_len = gb.show(LOG2_FRAME_SIZE);
        if buf_bit_size != (8 + padding_start + xma_frame_len + padding_end) as i32 {
            log(&format!(
                "XMA frame sizing incorrect: buf_bit_size {buf_bit_size} != 8 + {padding_start} + {xma_frame_len} + {padding_end}"
            ));
            return Decoded::Refused(INVALID_DATA);
        }
        self.save_bits(&gb, data, xma_frame_len as i32, log);
        if self.decode_frame(output, log) { Decoded::Frame } else { Decoded::None }
    }

    // save_bits, not appending: the frame from the start of the byte it starts in, so that
    // it reads from bit frame_offset. What is after it in frame_data stays as it was.
    fn save_bits(&mut self, gb: &BitReader, data: &[u8], len: i32, log: Log) {
        let count = gb.count();
        let frame_offset = count & 7;
        let buflen = (frame_offset + len + 7) >> 3;
        // Unreachable from xmaframes, whose frames are at least 2 bits and at most 0x7FFF:
        // the C then goes on to read the previous frame's reader where it stopped.
        if len <= 0 || buflen as usize > MAX_FRAMESIZE {
            request_sample(log, "Too small input buffer");
            return;
        }
        self.frame_offset = frame_offset;
        self.num_saved_bits = frame_offset + len;
        let bits = self.num_saved_bits as usize;
        let start = (count >> 3) as usize;
        let whole = bits / 8;
        let there = data.len().saturating_sub(start).min(whole);
        self.frame_data[..there].copy_from_slice(&data[start..start + there]);
        self.frame_data[there..whole].fill(0);
        let rest = bits % 8;
        if rest != 0 {
            let byte = data.get(start + whole).copied().unwrap_or(0);
            self.frame_data[whole] = byte & !(0xFFu8 >> rest);
        }
    }

    fn decode_frame(&mut self, output: &mut [[f32; SAMPLES]; 2], log: Log) -> bool {
        // Taken out while it is read, so that the decoder's state can change meanwhile.
        let frame_data = std::mem::take(&mut self.frame_data);
        let mut gb = BitReader::new(&frame_data, self.num_saved_bits);
        gb.skip(self.frame_offset as u32);
        let got = self.decode_frame_from(&mut gb, output, log);
        self.frame_data = frame_data;
        got
    }

    fn decode_frame_from(&mut self, gb: &mut BitReader, output: &mut [[f32; SAMPLES]; 2], log: Log) -> bool {
        let len = if self.len_prefix { gb.get(LOG2_FRAME_SIZE) as i32 } else { 0 };

        if self.decode_tilehdr(gb, log).is_err() {
            return false;
        }

        // The postprocessing transform.
        if self.nb_channels > 1 && gb.get1() != 0 && gb.get1() != 0 {
            for _ in 0..self.nb_channels * self.nb_channels {
                gb.skip(4);
            }
        }

        if self.dynamic_range_compression {
            self.drc_gain = gb.get(8) as u8;
        }

        if gb.get1() != 0 {
            if gb.get1() != 0 {
                self.trim_start = gb.get(av_log2(self.samples_per_frame as u32 * 2)) as u16;
            }
            if gb.get1() != 0 {
                self.trim_end = gb.get(av_log2(self.samples_per_frame as u32 * 2)) as u16;
            }
        } else {
            self.trim_start = 0;
            self.trim_end = 0;
        }

        self.parsed_all_subframes = false;
        for c in 0..self.nb_channels {
            self.channel[c].decoded_samples = 0;
            self.channel[c].cur_subframe = 0;
            self.channel[c].reuse_sf = 0;
        }

        while !self.parsed_all_subframes {
            if self.decode_subframe(gb, log).is_err() {
                return false;
            }
        }

        let spf = self.samples_per_frame as usize;
        for (to, out) in output.iter_mut().zip(&mut self.out) {
            to.copy_from_slice(&out[..spf]);
            // The second half of the IMDCT's output is the next frame's start.
            out.copy_within(spf..spf + spf / 2, 0);
        }

        if self.len_prefix {
            let consumed = gb.count() - self.frame_offset;
            if len != consumed + 2 {
                log(&format!("frame[{}] would have to skip {} bits", self.frame_num, len - consumed - 1));
                return true;
            }
            gb.skip_long(len - consumed - 1);
        } else {
            while gb.count() < self.num_saved_bits && gb.get1() == 0 {}
        }

        let _more_frames = gb.get1();
        self.frame_num = self.frame_num.wrapping_add(1);
        true
    }

    fn decode_subframe_length(&mut self, gb: &mut BitReader, offset: i32, log: Log) -> Result<i32, Failed> {
        if offset == self.samples_per_frame - self.min_samples_per_subframe {
            return Ok(self.min_samples_per_subframe);
        }
        if gb.left() < 1 {
            return Err(Failed);
        }
        let mut frame_len_shift = 0;
        if self.max_subframe_len_bit {
            if gb.get1() != 0 {
                frame_len_shift = 1 + gb.get_z(self.subframe_len_bits - 1);
            }
        } else {
            frame_len_shift = gb.get(self.subframe_len_bits);
        }
        let subframe_len = self.samples_per_frame >> frame_len_shift;
        if subframe_len < self.min_samples_per_subframe || subframe_len > self.samples_per_frame {
            log(&format!("broken frame: subframe_len {subframe_len}"));
            return Err(Failed);
        }
        Ok(subframe_len)
    }

    fn decode_tilehdr(&mut self, gb: &mut BitReader, log: Log) -> Result<(), Failed> {
        let mut num_samples = [0u16; MAX_CHANNELS];
        let mut contains_subframe = [false; MAX_CHANNELS];
        let mut channels_for_cur_subframe = self.nb_channels;
        let mut min_channel_len: i32 = 0;

        for c in 0..self.nb_channels {
            self.channel[c].num_subframes = 0;
        }

        let fixed_channel_layout = self.max_num_subframes == 1 || gb.get1() != 0;

        loop {
            for c in 0..self.nb_channels {
                contains_subframe[c] = if i32::from(num_samples[c]) == min_channel_len {
                    if fixed_channel_layout
                        || channels_for_cur_subframe == 1
                        || min_channel_len == self.samples_per_frame - self.min_samples_per_subframe
                    {
                        true
                    } else {
                        gb.get1() != 0
                    }
                } else {
                    false
                };
            }

            let subframe_len = self.decode_subframe_length(gb, min_channel_len, log)?;
            if subframe_len <= 0 {
                return Err(Failed);
            }

            min_channel_len += subframe_len;
            for c in 0..self.nb_channels {
                let chan = &mut self.channel[c];
                if contains_subframe[c] {
                    if chan.num_subframes as usize >= MAX_SUBFRAMES {
                        log("broken frame: num subframes > 31");
                        return Err(Failed);
                    }
                    chan.subframe_len[chan.num_subframes as usize] = subframe_len as u16;
                    num_samples[c] = num_samples[c].wrapping_add(subframe_len as u16);
                    chan.num_subframes += 1;
                    if i32::from(num_samples[c]) > self.samples_per_frame {
                        log("broken frame: channel len > samples_per_frame");
                        return Err(Failed);
                    }
                } else if i32::from(num_samples[c]) <= min_channel_len {
                    if i32::from(num_samples[c]) < min_channel_len {
                        channels_for_cur_subframe = 0;
                        min_channel_len = i32::from(num_samples[c]);
                    }
                    channels_for_cur_subframe += 1;
                }
            }
            if min_channel_len >= self.samples_per_frame {
                break;
            }
        }

        for c in 0..self.nb_channels {
            let mut offset = 0u16;
            for i in 0..self.channel[c].num_subframes as usize {
                self.channel[c].subframe_offset[i] = offset;
                offset = offset.wrapping_add(self.channel[c].subframe_len[i]);
            }
        }
        Ok(())
    }

    fn decode_decorrelation_matrix(&mut self, gb: &mut BitReader, group: usize) {
        let statics = &*STATICS;
        let nb = self.nb_channels;
        let chgroup = &mut self.chgroup[group];
        let n = chgroup.num_channels as usize;
        let mut rotation_offset = [0i8; MAX_CHANNELS * MAX_CHANNELS];
        chgroup.decorrelation_matrix[..nb * nb].fill(0.0);

        for offset in rotation_offset.iter_mut().take((n * n.saturating_sub(1)) >> 1) {
            *offset = gb.get(6) as i8;
        }
        for i in 0..n {
            chgroup.decorrelation_matrix[n * i + i] = if gb.get1() != 0 { 1.0 } else { -1.0 };
        }
        let mut offset = 0;
        for i in 1..n {
            for x in 0..i {
                for y in 0..i + 1 {
                    let v1 = chgroup.decorrelation_matrix[x * n + y];
                    let v2 = chgroup.decorrelation_matrix[i * n + y];
                    let k = rotation_offset[offset + x] as usize;
                    let (sinv, cosv) = if k < 32 {
                        (statics.sin64[k], statics.sin64[32 - k])
                    } else {
                        (statics.sin64[64 - k], -statics.sin64[k - 32])
                    };
                    chgroup.decorrelation_matrix[y + x * n] = (v1 * sinv) - (v2 * cosv);
                    chgroup.decorrelation_matrix[y + i * n] = (v1 * cosv) + (v2 * sinv);
                }
            }
            offset += i;
        }
    }

    fn decode_channel_transform(&mut self, gb: &mut BitReader, log: Log) -> Result<(), Failed> {
        self.num_chgroups = 0;
        if self.nb_channels <= 1 {
            return Ok(());
        }
        let mut remaining_channels = self.channels_for_cur_subframe;
        if gb.get1() != 0 {
            request_sample(log, "Channel transform bit");
            return Err(Failed);
        }

        while remaining_channels != 0 && self.num_chgroups < self.channels_for_cur_subframe {
            let g = self.num_chgroups;
            self.chgroup[g].num_channels = 0;
            self.chgroup[g].transform = 0;
            let mut data = 0;

            if remaining_channels > 2 {
                for i in 0..self.channels_for_cur_subframe {
                    let channel_idx = self.channel_indexes_for_cur_subframe[i];
                    if self.channel[channel_idx].grouped == 0 && gb.get1() != 0 {
                        self.chgroup[g].num_channels += 1;
                        self.channel[channel_idx].grouped = 1;
                        self.chgroup[g].channel_data[data] = channel_idx;
                        data += 1;
                    }
                }
            } else {
                self.chgroup[g].num_channels = remaining_channels as u8;
                for i in 0..self.channels_for_cur_subframe {
                    let channel_idx = self.channel_indexes_for_cur_subframe[i];
                    if self.channel[channel_idx].grouped == 0 {
                        self.chgroup[g].channel_data[data] = channel_idx;
                        data += 1;
                    }
                    self.channel[channel_idx].grouped = 1;
                }
            }

            let num_channels = self.chgroup[g].num_channels as usize;
            if num_channels == 2 {
                if gb.get1() != 0 {
                    if gb.get1() != 0 {
                        request_sample(log, "Unknown channel transform type");
                        return Err(Failed);
                    }
                } else {
                    let chgroup = &mut self.chgroup[g];
                    chgroup.transform = 1;
                    let m = if self.nb_channels == 2 {
                        [1.0, -1.0, 1.0, 1.0]
                    } else {
                        [0.70703125, -0.70703125, 0.70703125, 0.70703125]
                    };
                    chgroup.decorrelation_matrix[..4].copy_from_slice(&m);
                }
            } else if num_channels > 2 && gb.get1() != 0 {
                self.chgroup[g].transform = 1;
                if gb.get1() != 0 {
                    self.decode_decorrelation_matrix(gb, g);
                } else if num_channels > 6 {
                    request_sample(log, "Coupled channels > 6");
                } else {
                    let at = DEFAULT_DECORRELATION[num_channels];
                    let size = num_channels * num_channels;
                    self.chgroup[g].decorrelation_matrix[..size]
                        .copy_from_slice(&DEFAULT_DECORRELATION_MATRICES[at..at + size]);
                }
            }

            if self.chgroup[g].transform != 0 {
                if gb.get1() == 0 {
                    for i in 0..self.num_bands {
                        self.chgroup[g].transform_band[i] = gb.get1() as i8;
                    }
                } else {
                    self.chgroup[g].transform_band[..self.num_bands].fill(1);
                }
            }
            remaining_channels = remaining_channels.wrapping_sub(num_channels);
            self.num_chgroups += 1;
        }
        Ok(())
    }

    // ff_wma_get_large_val
    fn large_val(gb: &mut BitReader) -> u32 {
        let mut n_bits = 8;
        if gb.get1() != 0 {
            n_bits += 8;
            if gb.get1() != 0 {
                n_bits += 8;
                if gb.get1() != 0 {
                    n_bits += 7;
                }
            }
        }
        gb.get_long(n_bits)
    }

    fn decode_coeffs(&mut self, gb: &mut BitReader, c: usize, log: Log) {
        // The integers 0 to 15 as floats.
        const FVAL: [u32; 16] = [
            0x00000000, 0x3f800000, 0x40000000, 0x40400000, 0x40800000, 0x40a00000, 0x40c00000, 0x40e00000, 0x41000000,
            0x41100000, 0x41200000, 0x41300000, 0x41400000, 0x41500000, 0x41600000, 0x41700000,
        ];
        let statics = &*STATICS;
        let vlctable = gb.get1() as usize;
        let table = &statics.coef[vlctable];
        let (run, level): (&[u16], &[f32]) =
            if vlctable != 0 { (&COEF1_RUN, &COEF1_LEVEL) } else { (&COEF0_RUN, &COEF0_LEVEL) };
        let mut rl_mode = false;
        let mut cur_coeff: i32 = 0;
        let mut num_zeros: i32 = 0;
        let coeffs = self.channel[c].coeffs;
        let num_vec_coeffs = i32::from(self.channel[c].num_vec_coeffs);
        let out = &mut self.out[c];

        while (self.transmit_num_vec_coeffs || !rl_mode) && cur_coeff + 3 < num_vec_coeffs {
            let mut vals = [0u32; 4];
            let idx = gb.vlc(&statics.vec4, VLCBITS, VEC4MAXDEPTH);
            if idx < 0 {
                for i in (0..4).step_by(2) {
                    let idx = gb.vlc(&statics.vec2, VLCBITS, VEC2MAXDEPTH);
                    if idx < 0 {
                        let mut v0 = gb.vlc(&statics.vec1, VLCBITS, VEC1MAXDEPTH) as u32;
                        if v0 == HUFF_VEC1_SIZE as u32 - 1 {
                            v0 = v0.wrapping_add(Self::large_val(gb));
                        }
                        let mut v1 = gb.vlc(&statics.vec1, VLCBITS, VEC1MAXDEPTH) as u32;
                        if v1 == HUFF_VEC1_SIZE as u32 - 1 {
                            v1 = v1.wrapping_add(Self::large_val(gb));
                        }
                        vals[i] = (v0 as f32).to_bits();
                        vals[i + 1] = (v1 as f32).to_bits();
                    } else {
                        let idx = idx as u32;
                        vals[i] = FVAL[(idx >> 4) as usize & 0xF];
                        vals[i + 1] = FVAL[(idx & 0xF) as usize];
                    }
                }
            } else {
                let idx = idx as u32;
                vals[0] = FVAL[(idx >> 12) as usize & 0xF];
                vals[1] = FVAL[((idx >> 8) & 0xF) as usize];
                vals[2] = FVAL[((idx >> 4) & 0xF) as usize];
                vals[3] = FVAL[(idx & 0xF) as usize];
            }

            for value in vals {
                let at = coeffs + cur_coeff as usize;
                if value != 0 {
                    let sign = gb.get1().wrapping_sub(1);
                    out[at] = f32::from_bits(value ^ (sign << 31));
                    num_zeros = 0;
                } else {
                    out[at] = 0.0;
                    num_zeros += 1;
                    rl_mode |= num_zeros > i32::from(self.subframe_len) >> 8;
                }
                cur_coeff += 1;
            }
        }

        let subframe_len = i32::from(self.subframe_len);
        if cur_coeff < subframe_len {
            out[coeffs + cur_coeff as usize..coeffs + subframe_len as usize].fill(0.0);
            // ff_wma_run_level_decode, version 1: run-level codes with an escape
            let coef_mask = (subframe_len - 1) as usize;
            let block = &mut out[coeffs..coeffs + subframe_len as usize];
            let mut offset = cur_coeff;
            while offset < subframe_len {
                let code = gb.vlc(table, VLCBITS, VLCMAX);
                if code > 1 {
                    offset += i32::from(run[code as usize]);
                    let sign = gb.get1().wrapping_sub(1);
                    block[offset as usize & coef_mask] =
                        f32::from_bits(level[code as usize].to_bits() ^ (sign & 0x8000_0000));
                } else if code == 1 {
                    break; // the end of the block
                } else {
                    let level = Self::large_val(gb) as i32;
                    if gb.get1() != 0 {
                        if gb.get1() != 0 {
                            if gb.get1() != 0 {
                                log("broken escape sequence");
                                return;
                            }
                            offset += gb.get(self.esc_len) as i32 + 4;
                        } else {
                            offset += gb.get(2) as i32 + 1;
                        }
                    }
                    let sign = gb.get1() as i32 - 1;
                    block[offset as usize & coef_mask] = ((level ^ sign).wrapping_sub(sign)) as f32;
                }
                offset += 1;
            }
            if offset > subframe_len {
                log(&format!("overflow ({offset} > {subframe_len}) in spectral RLE, ignoring"));
            }
        }
    }

    fn decode_scale_factors(&mut self, gb: &mut BitReader, log: Log) -> Result<(), Failed> {
        let statics = &*STATICS;
        for i in 0..self.channels_for_cur_subframe {
            let c = self.channel_indexes_for_cur_subframe[i];
            let num_bands = self.num_bands;
            let table_idx = self.table_idx;
            let channel = &mut self.channel[c];
            let current = usize::from(channel.scale_factor_idx == 0);
            channel.scale_factors = current;

            if channel.reuse_sf != 0 {
                let sf_offsets = self.sf_offsets[table_idx][channel.table_idx as usize];
                let saved = channel.saved_scale_factors[channel.scale_factor_idx as usize];
                for b in 0..num_bands {
                    channel.saved_scale_factors[current][b] = saved[sf_offsets[b] as usize];
                }
            }

            if channel.cur_subframe == 0 || gb.get1() != 0 {
                if channel.reuse_sf == 0 {
                    channel.scale_factor_step = gb.get(2) as i8 + 1;
                    let mut val = 45 / i32::from(channel.scale_factor_step);
                    for b in 0..num_bands {
                        val += gb.vlc(&statics.sf, SCALEVLCBITS, SCALEMAXDEPTH);
                        channel.saved_scale_factors[current][b] = val;
                    }
                } else {
                    let mut b = 0;
                    while b < num_bands {
                        let idx = gb.vlc(&statics.sf_rl, VLCBITS, SCALERLMAXDEPTH);
                        let (skip, val, sign);
                        if idx == 0 {
                            let code = gb.get(14);
                            val = (code >> 6) as i32;
                            sign = (code & 1) as i32 - 1;
                            skip = ((code & 0x3f) >> 1) as usize;
                        } else if idx == 1 {
                            break;
                        } else {
                            // Never -1: the table's codes are complete.
                            skip = usize::from(SCALE_RL_RUN.get(idx as usize).copied().unwrap_or(0));
                            val = i32::from(SCALE_RL_LEVEL.get(idx as usize).copied().unwrap_or(0));
                            sign = gb.get1() as i32 - 1;
                        }
                        b += skip;
                        if b >= num_bands {
                            log("invalid scale factor coding");
                            return Err(Failed);
                        }
                        let factor = &mut channel.saved_scale_factors[current][b];
                        *factor = factor.wrapping_add((val ^ sign).wrapping_sub(sign));
                        b += 1;
                    }
                }
                channel.scale_factor_idx = i8::from(channel.scale_factor_idx == 0);
                channel.table_idx = table_idx as u8;
                channel.reuse_sf = 1;
            }

            let factors = &channel.saved_scale_factors[current][..num_bands];
            channel.max_scale_factor = factors.iter().copied().fold(factors[0], i32::max);
        }
        Ok(())
    }

    fn inverse_channel_transform(&mut self) {
        for i in 0..self.num_chgroups {
            let group = self.chgroup[i];
            if group.transform == 0 {
                continue;
            }
            let num_channels = group.num_channels as usize;
            for band in 0..self.num_bands {
                let start = self.sfb_offsets[self.table_idx][band] as usize;
                let end = (self.sfb_offsets[self.table_idx][band + 1] as usize).min(self.subframe_len as usize);
                if group.transform_band[band] == 1 {
                    for y in start..end {
                        let mut data = [0f32; MAX_CHANNELS];
                        for (k, &ch) in group.channel_data[..num_channels].iter().enumerate() {
                            data[k] = self.out[ch][self.channel[ch].coeffs + y];
                        }
                        let mut mat = 0;
                        for &ch in &group.channel_data[..num_channels] {
                            let mut sum = 0f32;
                            for value in &data[..num_channels] {
                                sum += *value * group.decorrelation_matrix[mat];
                                mat += 1;
                            }
                            let coeffs = self.channel[ch].coeffs;
                            self.out[ch][coeffs + y] = sum;
                        }
                    }
                } else if self.nb_channels == 2 {
                    // vector_fmul_scalar by 181/128, in place
                    for &ch in &group.channel_data[..2] {
                        let coeffs = self.channel[ch].coeffs;
                        for value in &mut self.out[ch][coeffs + start..coeffs + end] {
                            *value *= (181.0f64 / 128.0) as f32;
                        }
                    }
                }
            }
        }
    }

    // wmapro_window: the sine window over the overlap with the block before.
    fn window(&mut self) {
        let statics = &*STATICS;
        for i in 0..self.channels_for_cur_subframe {
            let c = self.channel_indexes_for_cur_subframe[i];
            let mut winlen = self.channel[c].prev_block_len as usize;
            let mut start = self.channel[c].coeffs - (winlen >> 1);
            if (self.subframe_len as usize) < winlen {
                start += (winlen - self.subframe_len as usize) >> 1;
                winlen = self.subframe_len as usize;
            }
            let window = &statics.windows[av_log2(winlen as u32) as usize];
            let len = winlen >> 1;
            // vector_fmul_window(start, start, start + len, window, len), in place: the first
            // half against the second read backwards.
            let (first, second) = self.out[c][start..start + winlen].split_at_mut(len);
            let (rising, falling) = window[..winlen].split_at(len);
            for (k, (s0, s1)) in first.iter_mut().zip(second.iter_mut().rev()).enumerate() {
                let (a, b) = (*s0, *s1);
                let (wi, wj) = (rising[k], falling[len - 1 - k]);
                *s0 = a * wj - b * wi;
                *s1 = a * wi + b * wj;
            }
            self.channel[c].prev_block_len = self.subframe_len;
        }
    }

    fn decode_subframe(&mut self, gb: &mut BitReader, log: Log) -> Result<(), Failed> {
        let spf = self.samples_per_frame;
        let mut offset = spf;
        let mut subframe_len = spf;
        let mut total_samples = spf * self.nb_channels as i32;
        let mut transmit_coeffs = false;

        for i in 0..self.nb_channels {
            self.channel[i].grouped = 0;
            if offset > i32::from(self.channel[i].decoded_samples) {
                offset = i32::from(self.channel[i].decoded_samples);
                subframe_len =
                    i32::from(self.channel[i].subframe_len[self.channel[i].cur_subframe as usize % MAX_SUBFRAMES]);
            }
        }

        self.channels_for_cur_subframe = 0;
        for i in 0..self.nb_channels {
            let channel = &mut self.channel[i];
            let cur = channel.cur_subframe as usize % MAX_SUBFRAMES;
            total_samples -= i32::from(channel.decoded_samples);
            if offset == i32::from(channel.decoded_samples) && subframe_len == i32::from(channel.subframe_len[cur]) {
                total_samples -= i32::from(channel.subframe_len[cur]);
                channel.decoded_samples = channel.decoded_samples.wrapping_add(channel.subframe_len[cur]);
                self.channel_indexes_for_cur_subframe[self.channels_for_cur_subframe] = i;
                self.channels_for_cur_subframe += 1;
            }
        }

        if total_samples == 0 {
            self.parsed_all_subframes = true;
        }

        self.table_idx = av_log2((spf / subframe_len) as u32) as usize;
        self.num_bands = self.num_sfb[self.table_idx] as usize;

        offset += spf >> 1;
        for i in 0..self.channels_for_cur_subframe {
            let c = self.channel_indexes_for_cur_subframe[i];
            self.channel[c].coeffs = offset as usize;
        }

        self.subframe_len = subframe_len as i16;
        self.esc_len = av_log2((subframe_len - 1) as u32) + 1;

        // An extended header, skipped.
        if gb.get1() != 0 {
            let mut num_fill_bits = gb.get(2) as i32;
            if num_fill_bits == 0 {
                let len = gb.get(4);
                num_fill_bits = gb.get_z(len) as i32 + 1;
            }
            if num_fill_bits >= 0 {
                if gb.count() + num_fill_bits > self.num_saved_bits {
                    log("invalid number of fill bits");
                    return Err(Failed);
                }
                gb.skip_long(num_fill_bits);
            }
        }

        if gb.get1() != 0 {
            request_sample(log, "Reserved bit");
            return Err(Failed);
        }

        self.decode_channel_transform(gb, log)?;

        for i in 0..self.channels_for_cur_subframe {
            let c = self.channel_indexes_for_cur_subframe[i];
            self.channel[c].transmit_coefs = gb.get1() as u8;
            if self.channel[c].transmit_coefs != 0 {
                transmit_coeffs = true;
            }
        }

        if transmit_coeffs {
            let mut quant_step = ((90 * BITS_PER_SAMPLE) >> 4) as i32;

            self.transmit_num_vec_coeffs = gb.get1() != 0;
            if self.transmit_num_vec_coeffs {
                let num_bits = av_log2(((subframe_len + 3) / 4) as u32) + 1;
                for i in 0..self.channels_for_cur_subframe {
                    let c = self.channel_indexes_for_cur_subframe[i];
                    let num_vec_coeffs = (gb.get(num_bits) << 2) as i32;
                    if num_vec_coeffs > subframe_len {
                        log(&format!("num_vec_coeffs {num_vec_coeffs} is too large"));
                        return Err(Failed);
                    }
                    self.channel[c].num_vec_coeffs = num_vec_coeffs as u16;
                }
            } else {
                for i in 0..self.channels_for_cur_subframe {
                    let c = self.channel_indexes_for_cur_subframe[i];
                    self.channel[c].num_vec_coeffs = subframe_len as u16;
                }
            }

            let mut step = gb.get_signed(6);
            quant_step += step;
            if step == -32 || step == 31 {
                let sign = i32::from(step == 31) - 1;
                let mut quant = 0;
                while gb.count() + 5 < self.num_saved_bits && {
                    step = gb.get(5) as i32;
                    step == 31
                } {
                    quant += 31;
                }
                quant_step += ((quant + step) ^ sign) - sign;
            }

            if self.channels_for_cur_subframe == 1 {
                let c = self.channel_indexes_for_cur_subframe[0];
                self.channel[c].quant_step = quant_step;
            } else {
                let modifier_len = gb.get(3);
                for i in 0..self.channels_for_cur_subframe {
                    let c = self.channel_indexes_for_cur_subframe[i];
                    self.channel[c].quant_step = quant_step;
                    if gb.get1() != 0 {
                        if modifier_len != 0 {
                            self.channel[c].quant_step += gb.get(modifier_len) as i32 + 1;
                        } else {
                            self.channel[c].quant_step += 1;
                        }
                    }
                }
            }

            self.decode_scale_factors(gb, log)?;
        }

        for i in 0..self.channels_for_cur_subframe {
            let c = self.channel_indexes_for_cur_subframe[i];
            if self.channel[c].transmit_coefs != 0 && gb.count() < self.num_saved_bits {
                self.decode_coeffs(gb, c, log);
            } else {
                let coeffs = self.channel[c].coeffs;
                self.out[c][coeffs..coeffs + subframe_len as usize].fill(0.0);
            }
        }

        if transmit_coeffs {
            let transform = av_log2(subframe_len as u32) as usize - BLOCK_MIN_BITS as usize;
            self.inverse_channel_transform();
            for i in 0..self.channels_for_cur_subframe {
                let c = self.channel_indexes_for_cur_subframe[i];
                let channel = self.channel[c];
                let factors = &channel.saved_scale_factors[channel.scale_factors];
                for (b, factor) in factors.iter().enumerate().take(self.num_bands) {
                    let start = self.sfb_offsets[self.table_idx][b] as usize;
                    let end = (self.sfb_offsets[self.table_idx][b + 1] as i32).min(subframe_len) as usize;
                    let exp =
                        channel.quant_step - (channel.max_scale_factor - factor) * i32::from(channel.scale_factor_step);
                    // ff_exp10(exp / 20.0): exp2 of the double, narrowed
                    let quant = ((std::f64::consts::LOG2_10 * (f64::from(exp) / 20.0)).exp2()) as f32;
                    let source = &self.out[c][channel.coeffs + start..channel.coeffs + end];
                    for (to, from) in self.tmp[start..end].iter_mut().zip(source) {
                        *to = *from * quant;
                    }
                }
                let len = subframe_len as usize;
                self.transforms.mdcts[transform]
                    .inverse(&mut self.out[c][channel.coeffs..channel.coeffs + len], &self.tmp[..len]);
            }
        }

        self.window();

        for i in 0..self.channels_for_cur_subframe {
            let c = self.channel_indexes_for_cur_subframe[i];
            if self.channel[c].cur_subframe >= self.channel[c].num_subframes {
                log("broken subframe");
                return Err(Failed);
            }
            self.channel[c].cur_subframe += 1;
        }
        Ok(())
    }
}
