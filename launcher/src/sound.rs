// The two sounds of the game's own menus, for the launcher's: the tick as another entry is
// reached and the one a choice or going back makes. Nothing of them is shipped; they are
// read out of the installed game (fastfile.rs), so a launcher with no game beside it is
// silent. Their samples are XMA in 2 KB packets, decoded frame by frame with the game's
// own decoder (the wmapro crate).
//
// The menus do not name these sounds but two aliases, "mouse_over" and "mouse_click",
// which add the volume and pitch they are played at; the figures below are those
// aliases'. They are played as the title plays them, measured from its own output (the
// six channels it hands the audio driver, src/runtime/kernel/audio.rs): in the front left
// and right of a 5.1 stream, which the system mixes down for its speakers as it does the
// game's, at TITLE_FRONT times the alias's volume times the game's volume -- the one the
// player set in its options, kept in the profile (profile.rs), which the title gives its
// snd_volume, a dvar of 0 to 0.8 that starts at 0.8. TITLE_FRONT is the same to four
// digits at volumes 0.24, 0.5 and 1.0, the last capped to 0.8.
#![deny(unsafe_code)]

use crate::{fastfile, profile, settings};
use std::path::Path;
use std::sync::Mutex;

const PACKET: usize = 2048;
const PACKET_HEADER: usize = 4;
// What the decoder is opened at; the sound's own rate is its header's, and the sound is
// resampled from that.
const DECODER_RATE: i32 = 44100;
// The bytes the decoder may read past a frame (FFmpeg's AV_INPUT_BUFFER_PADDING_SIZE).
const PADDING: usize = 64;

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

struct Alias {
    sound: &'static str,
    volume: f32,
    pitch: f32,
}

const ALIASES: [Alias; 2] = [
    Alias { sound: "user_interface/ui_over_v2", volume: 0.27, pitch: 1.3 }, // mouse_over
    Alias { sound: "user_interface/ui_select_hz_1", volume: 0.21, pitch: 1.0 }, // mouse_click
];

const FASTFILE: &str = "code_post_gfx.ff";
const DEVICE_RATE: i32 = 48000;
// The title's level for a menu sound in each front channel, per unit of its volume.
const TITLE_FRONT: f32 = 0.7533;
// snd_volume's range, and where it starts.
const MOST_VOLUME: f32 = 0.8;
// The 5.1 stream's channels, as the game's: FL FR FC LFE BL BR.
const CHANNELS: usize = 6;
// How many sounds at once: the click rings for two seconds, and the ticks of a pointer
// crossing the entries come over it.
const VOICES: usize = 4;

#[derive(Clone, Copy)]
#[repr(i32)]
pub enum Clip {
    Over,
    Click,
}

// What is playing: the clips as the device takes them, and SDL's streams, as numbers.
struct Playing {
    loaded: bool,
    clips: [Vec<f32>; 2],
    voices: [usize; VOICES],
    next: usize,
}

static PLAYING: Mutex<Playing> =
    Mutex::new(Playing { loaded: false, clips: [Vec::new(), Vec::new()], voices: [0; VOICES], next: 0 });

fn playing() -> std::sync::MutexGuard<'static, Playing> {
    PLAYING.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// The clips, at the device's rate, or None when the game is not there or not as known.
fn read() -> Option<[Vec<f32>; 2]> {
    let packed = std::fs::read(Path::new(crate::setup::GAME_FOLDER).join(FASTFILE)).ok()?;
    let zone = fastfile::zone(&packed)?;
    let mut clips: [Vec<f32>; 2] = [Vec::new(), Vec::new()];
    for (clip, alias) in clips.iter_mut().zip(&ALIASES) {
        let found = fastfile::find(&zone, alias.sound)?;
        let mut samples = decode(&zone[found.at..found.at + found.size]);
        if samples.is_empty() {
            return None;
        }
        for sample in &mut samples {
            *sample *= alias.volume;
        }
        // A higher pitch is the same samples played faster.
        *clip = device::convert(&samples, (found.rate as f32 * alias.pitch) as i32, DEVICE_RATE)?;
    }
    Some(clips)
}

/// Reads the sounds from game/ and opens the sound device. Called again once an install
/// has put the game there; nothing to do when they are read.
pub fn load() {
    let mut playing = playing();
    if playing.loaded || !settings::sounds() {
        return;
    }
    let Some(clips) = read() else { return };
    playing.clips = clips;
    let Some(voices) = device::open(DEVICE_RATE, CHANNELS as i32) else { return };
    playing.voices = voices;
    playing.loaded = true;
}

pub fn play(clip: Clip) {
    let mut playing = playing();
    if !playing.loaded {
        return;
    }
    let voice = playing.voices[playing.next];
    playing.next = (playing.next + 1) % VOICES;
    if voice == 0 {
        return;
    }
    // The game's volume now, as it may have changed since the last sound.
    let volume = profile::volume().filter(|volume| volume.is_finite()).unwrap_or(MOST_VOLUME).clamp(0.0, MOST_VOLUME);
    let gain = TITLE_FRONT * volume;
    let mut frames = vec![0f32; playing.clips[clip as usize].len() * CHANNELS];
    for (frame, &sample) in frames.chunks_exact_mut(CHANNELS).zip(&playing.clips[clip as usize]) {
        frame[0] = sample * gain;
        frame[1] = sample * gain;
    }
    device::play(voice, &frames);
}

// SDL's side: the resampling, the device and its streams.
#[allow(unsafe_code)]
mod device {
    use super::VOICES;
    use sdl3_sys::everything as sdl;
    use std::ptr;

    fn spec(rate: i32, channels: i32) -> sdl::SDL_AudioSpec {
        sdl::SDL_AudioSpec { format: sdl::SDL_AUDIO_F32, channels, freq: rate }
    }

    /// The samples, one channel, at `from` played at `to`.
    pub fn convert(samples: &[f32], from: i32, to: i32) -> Option<Vec<f32>> {
        let (from, to) = (spec(from, 1), spec(to, 1));
        let mut converted: *mut u8 = ptr::null_mut();
        let mut bytes = 0;
        // SAFETY: the samples as bytes, and SDL's buffer, copied and freed.
        unsafe {
            if !sdl::SDL_ConvertAudioSamples(
                &from,
                samples.as_ptr().cast(),
                size_of_val(samples) as i32,
                &to,
                &mut converted,
                &mut bytes,
            ) {
                return None;
            }
            let out = std::slice::from_raw_parts(converted.cast::<f32>(), bytes as usize / 4).to_vec();
            sdl::SDL_free(converted.cast());
            Some(out)
        }
    }

    /// The streams, each its own on the default device, which mixes them; None without
    /// SDL's audio. A stream that would not open is 0.
    pub fn open(rate: i32, channels: i32) -> Option<[usize; VOICES]> {
        let spec = spec(rate, channels);
        let mut voices = [0usize; VOICES];
        // SAFETY: SDL's calls.
        unsafe {
            if !sdl::SDL_InitSubSystem(sdl::SDL_INIT_AUDIO) {
                return None;
            }
            for voice in &mut voices {
                let stream = sdl::SDL_OpenAudioDeviceStream(
                    sdl::SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK,
                    &spec,
                    None,
                    ptr::null_mut(),
                );
                if !stream.is_null() {
                    sdl::SDL_ResumeAudioStreamDevice(stream);
                }
                *voice = stream as usize;
            }
        }
        Some(voices)
    }

    /// The oldest voice, whatever is left of what it was playing, given the clip's frames.
    pub fn play(voice: usize, samples: &[f32]) {
        let stream = voice as *mut sdl::SDL_AudioStream;
        // SAFETY: a stream open() opened, which stays open.
        unsafe {
            sdl::SDL_ClearAudioStream(stream);
            sdl::SDL_PutAudioStreamData(stream, samples.as_ptr().cast(), size_of_val(samples) as i32);
        }
    }
}
