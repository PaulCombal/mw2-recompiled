// A sound of the game's own menus, found in the fastfile both titles load before their
// menus, code_post_gfx.ff (sound.rs plays it). A fastfile is a short header and one zlib
// stream; inflated, it is the title's assets one after another, each a structure followed
// by what its pointers led to. A loaded sound is
//
//     name pointer, XAUDIOPACKET (buffer pointer, buffer size, loops, context),
//     XAUDIOSOURCEFORMAT (sample type, streams, sample rate, channels),
//     length in milliseconds, seek table (count, pointer)
//
// and then the name's characters, the buffer and the seek table. The buffer is XMA in
// 2 KB packets, the same the title hands the console's decoder while it runs
// (src/runtime/kernel/xma.rs).
//
// A sound is looked up by its name's characters rather than by walking the assets before
// it, which would take the layout of every kind of asset there is. What is found is
// checked for being a sound before it is believed.
#![deny(unsafe_code)]

use mw2_install::inflate;

// An unsigned fastfile of this version that streams no images: the magic, the version, a
// flag, when it was made, the language, the count of streamed images and two sizes; the
// zlib stream is next.
const MAGIC: [u8; 12] = *b"IWffu100\x00\x00\x01\x0D";
const STREAMED_IMAGES: usize = 0x19;
const ZLIB: usize = 0x25;

const PACKET: usize = 2048;

fn big32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// The assets of a fastfile of this kind, as zlib would inflate them; None for any other
/// file, or one zlib would refuse.
pub fn zone(packed: &[u8]) -> Option<Vec<u8>> {
    if packed.len() <= ZLIB || packed[..MAGIC.len()] != MAGIC || big32(packed, STREAMED_IMAGES) != 0 {
        return None;
    }
    inflate::unzlib(&packed[ZLIB..], usize::MAX)
}

/// A sound found: where its buffer starts in the zone, its size, and its sample rate.
#[derive(Debug, PartialEq, Eq)]
pub struct Sound {
    pub at: usize,
    pub size: usize,
    pub rate: i32,
}

/// The sound of that name: XMA, one stream of one channel, in whole packets that are all
/// there, at a rate from 8 to 48 kHz.
pub fn find(zone: &[u8], name: &str) -> Option<Sound> {
    // The structure's last field is the seek table's pointer, which the file leaves as -1
    // for "what follows"; the name follows it.
    const STRUCTURE: usize = 0xA4;
    const BUFFER_SIZE: usize = 0x0C;
    const SAMPLE_TYPE: usize = 0x60;
    const STREAMS: usize = 0x64;
    const RATE: usize = 0x68;
    const CHANNELS: usize = 0x6C;
    let mut key = vec![0xFF; 4];
    key.extend_from_slice(name.as_bytes());
    key.push(0);
    let found = STRUCTURE + zone.get(STRUCTURE..)?.windows(key.len()).position(|window| window == key)?;
    let characters = found + 4;
    let structure = characters - STRUCTURE;
    let at = characters + name.len() + 1;
    let size = big32(zone, structure + BUFFER_SIZE) as usize;
    let rate = big32(zone, structure + RATE) as i32;
    if zone[structure + SAMPLE_TYPE] != 4 || zone[structure + STREAMS] != 1 || zone[structure + CHANNELS] != 1 {
        return None;
    }
    if size == 0 || !size.is_multiple_of(PACKET) || size > zone.len() - at {
        return None;
    }
    if !(8000..=48000).contains(&rate) {
        return None;
    }
    Some(Sound { at, size, rate })
}
