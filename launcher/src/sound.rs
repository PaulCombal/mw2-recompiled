// The menu sounds' samples (sound.cpp finds the sounds and plays them): XMA in 2 KB
// packets, decoded frame by frame with the game's own decoder (the wmapro crate).

use std::ffi::c_void;

const PACKET: usize = 2048;
const PACKET_HEADER: usize = 4;
// What the decoder is opened at; the sound's own rate is its header's, and sound.cpp
// resamples from that.
const DECODER_RATE: i32 = 44100;
// The bytes the decoder may read past a frame (FFmpeg's AV_INPUT_BUFFER_PADDING_SIZE).
const PADDING: usize = 64;

type SampleSink = extern "C" fn(context: *mut c_void, samples: *const f32, count: usize);

fn decode(packets: &[u8]) -> Vec<f32> {
    let mut samples = Vec::new();
    if packets.len() < PACKET_HEADER {
        return samples;
    }
    // The packets' frames end to end: a frame runs on into the next packet.
    let bits: Vec<u8> =
        packets.chunks(PACKET).flat_map(|packet| packet.get(PACKET_HEADER..).unwrap_or_default()).copied().collect();
    let end = bits.len() * 8;
    let bit = |at: usize| (bits[at >> 3] >> (7 - (at & 7))) & 1;

    let mut decoder = wmapro::Decoder::new(DECODER_RATE, 1);
    let mut output = [[0f32; wmapro::SAMPLES]; 2];
    let mut quiet = |_: &str| {};
    // Where the first packet's first frame starts, from its header.
    let header = u32::from_be_bytes([packets[0], packets[1], packets[2], packets[3]]);
    let mut at = ((header >> 11) & 0x7FFF) as usize;
    let mut one = Vec::new();
    while at + 15 <= end {
        // A frame starts with its length in bits, itself included; all ones says
        // there are no more.
        let length = (0..15).fold(0usize, |length, i| length << 1 | bit(at + i) as usize);
        if length < 15 || length == 0x7FFF || at + length > end {
            break;
        }
        // The decoder takes a frame behind a byte that says how many bits of padding
        // are before it and after.
        let size = 1 + length.div_ceil(8);
        one.clear();
        one.resize(size + PADDING, 0);
        for i in 0..length {
            if bit(at + i) != 0 {
                one[1 + (i >> 3)] |= 0x80 >> (i & 7);
            }
        }
        one[0] = (((size * 8 - 8 - length) & 7) << 2) as u8;
        at += length;
        match decoder.decode(&one, size, &mut output, &mut quiet) {
            wmapro::Decoded::Frame => samples.extend_from_slice(&output[0]),
            wmapro::Decoded::None => {}
            wmapro::Decoded::Refused(_) => break,
        }
    }
    samples
}

/// Decodes `size` bytes of XMA packets, one channel, and hands `sink` the samples, once,
/// or not at all when no frame decoded.
///
/// # Safety
/// `packets` points at `size` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mw2l_decode_xma(packets: *const u8, size: usize, sink: SampleSink, context: *mut c_void) {
    if packets.is_null() {
        return;
    }
    // SAFETY: the caller's bytes.
    let samples = decode(unsafe { std::slice::from_raw_parts(packets, size) });
    if !samples.is_empty() {
        sink(context, samples.as_ptr(), samples.len());
    }
}
