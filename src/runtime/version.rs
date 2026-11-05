// Which release this build is, and which other copies it can play with.

use std::ffi::CStr;

/// The release tag the build is published as, "v0.8.2", or "" for a build that is
/// no release (cargo xtask's --release-tag).
pub const RELEASE: &CStr = match CStr::from_bytes_with_nul(concat!(env!("MW2_RELEASE_TAG"), "\0").as_bytes()) {
    Ok(tag) => tag,
    Err(_) => panic!("a release tag has no NUL"),
};

/// What copies of the same release share and copies of another do not: 1 to 32767,
/// from the tag, or from "dev" for every build without one. Two releases may share
/// it, by a chance of one in 32767.
pub const NETWORK: u16 = network(if RELEASE.is_empty() { c"dev" } else { RELEASE });

// FNV-1a of the tag, folded to 15 bits: the title prints its protocol, which
// carries this, as a signed number.
const fn network(tag: &CStr) -> u16 {
    let tag = tag.to_bytes();
    let mut hash = 2166136261u32;
    let mut i = 0;
    while i < tag.len() {
        hash = (hash ^ tag[i] as u32).wrapping_mul(16777619);
        i += 1;
    }
    let folded = ((hash ^ (hash >> 15) ^ (hash >> 30)) & 0x7FFF) as u16;
    if folded != 0 { folded } else { 1 }
}
