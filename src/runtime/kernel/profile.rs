// The player's profile.
//
// MW2 keeps its own settings -- controls, video, and the brightness the player sets on
// the calibration screen at first boot -- in the two 1000-byte blobs the console reserves
// for a title in the player's profile, XPROFILE_TITLE_SPECIFIC1 and 2. It writes them
// through XamUserWriteProfileSettings and reads them back at start-up alongside six
// system settings.
//
// Answering that read with "nothing is set" describes a profile that has never been
// used, which is a fresh first boot every launch: the calibration screen comes back, and
// every setting the player changed is gone. The settings are kept in saves/profile.bin
// instead and handed straight back. Those of a profile signed in at another controller
// (signin.rs) are in saves/profile_<its id>.bin. Nothing here interprets them -- a
// setting is an id, a source and its bytes -- so whatever the title chooses to put in its
// blobs survives without this having to know what any of it means.

use super::content::save_root;
use super::misc::ERROR_SUCCESS;
use super::{complete_overlapped, stack_argument};
use crate::guest::{Register, kernel_import};
use crate::log::{c_string, format_text, log_error, log_kernel};
use crate::runtime::signin;
use std::collections::BTreeMap;
use std::ffi::CString;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

const ERROR_NO_SUCH_USER: u32 = 0x0525;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

// XUSER_PROFILE_SETTING, 40 bytes, as the title's own reader walks it: source at +0 --
// zero means "not set", and it takes that branch before it looks at anything else -- the
// user at +8, the id at +16, and then either the value itself at +32 or, for a byte blob,
// its size at +32 and where the bytes are at +36.
const STRIDE: u32 = 0x28;
const SOURCE: u32 = 0;
const USER: u32 = 8;
const ID: u32 = 16;
const VALUE: u32 = 32;
const DATA: u32 = 36;
const SOURCE_TITLE: u32 = 2;

// The type is the top nibble of the setting id: 6 is a byte blob, and everything else
// fits in the eight bytes at +32.
const BINARY: u32 = 6;

fn binary(id: u32) -> bool {
    (id >> 28) & 0xF == BINARY
}

// A blob is 1000 bytes here; the cap is only so a wild size cannot ask for a gigabyte.
const LARGEST_SETTING: u32 = 64 * 1024;

struct Setting {
    source: u32,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct Profile {
    settings: BTreeMap<u32, Setting>,
    file: PathBuf,
    read: bool,
}

// By file name.
static PROFILES: Mutex<BTreeMap<CString, Profile>> = Mutex::new(BTreeMap::new());

// The profile of whoever is at a controller, or None for nobody.
fn profile_of(profiles: &mut BTreeMap<CString, Profile>, user: u32) -> Option<&mut Profile> {
    let key = if user == 0 {
        c"profile.bin".to_owned()
    } else {
        let player = signin::at(user);
        if !player.signed_in {
            return None;
        }
        format_text!("profile_%012llx.bin", player.id)
    };
    let file = save_root().join(key.to_str().unwrap_or_default());
    let profile = profiles.entry(key).or_default();
    if profile.file.as_os_str().is_empty() {
        profile.file = file;
    }
    Some(profile)
}

// "MW2PROF1", a count, then id / source / length / bytes for each setting, each number
// four bytes in the host's order.
const MAGIC: &[u8; 8] = b"MW2PROF1";

fn file_text(profile: &Profile) -> CString {
    c_string(profile.file.as_os_str().as_encoded_bytes())
}

fn read_settings(bytes: &[u8]) -> Option<BTreeMap<u32, Setting>> {
    let mut at = 0usize;
    let mut take = |size: usize| {
        let taken = bytes.get(at..at + size)?;
        at += size;
        Some(taken)
    };
    let number = |bytes: &[u8]| u32::from_le_bytes(bytes.try_into().expect("four bytes"));
    if take(8)? != MAGIC {
        return None;
    }
    let count = number(take(4)?);
    let mut settings = BTreeMap::new();
    for _ in 0..count {
        let id = number(take(4)?);
        let source = number(take(4)?);
        let length = number(take(4)?);
        if length > LARGEST_SETTING {
            return None;
        }
        let bytes = take(length as usize)?.to_vec();
        settings.insert(id, Setting { source, bytes });
    }
    Some(settings)
}

fn load(profile: &mut Profile) -> &mut BTreeMap<u32, Setting> {
    if !profile.read {
        profile.read = true;
        if let Ok(bytes) = std::fs::read(&profile.file) {
            match read_settings(&bytes) {
                Some(settings) => {
                    profile.settings = settings;
                    log_kernel!(
                        "profile: %llu settings read from %s",
                        profile.settings.len() as u64,
                        file_text(profile).as_c_str()
                    );
                }
                None => log_error!("profile: %s is not readable, starting fresh", file_text(profile).as_c_str()),
            }
        }
    }
    &mut profile.settings
}

fn save(profile: &Profile) {
    let _ = std::fs::create_dir_all(save_root());
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&(profile.settings.len() as u32).to_le_bytes());
    for (id, setting) in &profile.settings {
        bytes.extend_from_slice(&id.to_le_bytes());
        bytes.extend_from_slice(&setting.source.to_le_bytes());
        bytes.extend_from_slice(&(setting.bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&setting.bytes);
    }
    if std::fs::write(&profile.file, bytes).is_err() {
        log_error!("profile: cannot write %s", file_text(profile).as_c_str());
    }
}

// (titleId, userIndex, xuidCount, xuids, settingCount, settingIds, bufferSize, buffer,
// overlapped). Callers size the buffer with a first call that passes none, then read
// into it.
kernel_import!(XamUserReadProfileSettings, |ctx, memory| {
    let user = ctx.r4.as_u32();
    let (count, ids) = (ctx.r7.as_u32(), ctx.r8.as_u32());
    let (buffer_size, buffer) = (ctx.r9.as_u32(), ctx.r10.as_u32());
    let overlapped = stack_argument(ctx, memory, 8);
    let id_at = |i: u32| if ids != 0 { memory.u32_at(ids.wrapping_add(i.wrapping_mul(4))) } else { 0 };

    let mut profiles = PROFILES.lock().unwrap_or_else(PoisonError::into_inner);
    // Nobody signed in there: nothing is set.
    let empty = BTreeMap::new();
    let settings = match profile_of(&mut profiles, user) {
        Some(profile) => &*load(profile),
        None => &empty,
    };

    // A blob does not fit in the entry, so it goes after the entries and the entry points
    // at it -- which means the size the caller needs depends on what is actually stored.
    let mut blob_bytes = 0u32;
    if ids != 0 {
        for i in 0..count {
            let id = id_at(i);
            if let Some(setting) = settings.get(&id).filter(|_| binary(id)) {
                blob_bytes = blob_bytes.wrapping_add(setting.bytes.len() as u32);
            }
        }
    }
    let required = 8u32.wrapping_add(count.wrapping_mul(STRIDE)).wrapping_add(blob_bytes);

    if buffer == 0 || buffer_size == 0 || memory.u32_at(buffer_size) < required {
        if buffer_size != 0 {
            memory.store_u32(buffer_size, required);
        }
        // A sizing call is not an operation to complete; report the shortfall so the
        // caller retries with a big enough buffer.
        ctx.r3.u64 = u64::from(ERROR_INSUFFICIENT_BUFFER);
        return;
    }

    memory.store_u32(buffer, count);
    memory.store_u32(buffer + 4, buffer.wrapping_add(8));
    let mut blob = buffer.wrapping_add(8).wrapping_add(count.wrapping_mul(STRIDE));
    for i in 0..count {
        let id = id_at(i);
        let address = buffer.wrapping_add(8).wrapping_add(i.wrapping_mul(STRIDE));
        memory.zero(address, STRIDE);
        memory.store_u32(address + USER, user);
        memory.store_u32(address + ID, id);
        // Not found: the source stays zero, not set.
        let Some(setting) = settings.get(&id) else { continue };
        memory.store_u32(address + SOURCE, setting.source);
        if binary(id) {
            let length = setting.bytes.len() as u32;
            memory.store_bytes(blob, &setting.bytes);
            memory.store_u32(address + VALUE, length);
            memory.store_u32(address + DATA, blob);
            blob = blob.wrapping_add(length);
        } else {
            // The value in the guest's own byte order, exactly as it was given.
            memory.store_bytes(address + VALUE, &setting.bytes[..setting.bytes.len().min(8)]);
        }
    }
    drop(profiles);

    complete_overlapped(memory, overlapped, ERROR_SUCCESS);
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

// (titleId, userIndex, settingCount, settings, overlapped)
kernel_import!(XamUserWriteProfileSettings, |ctx, memory| {
    let (count, entries, overlapped) = (ctx.r5.as_u32(), ctx.r6.as_u32(), ctx.r7.as_u32());

    let mut profiles = PROFILES.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(profile) = profile_of(&mut profiles, ctx.r4.as_u32()) else {
        drop(profiles);
        complete_overlapped(memory, overlapped, ERROR_NO_SUCH_USER);
        ctx.r3.u64 = u64::from(ERROR_NO_SUCH_USER);
        return;
    };
    let stored = load(profile);

    let mut kept = 0u32;
    if entries != 0 {
        for i in 0..count {
            let address = entries.wrapping_add(i.wrapping_mul(STRIDE));
            let id = memory.u32_at(address + ID);
            if id == 0 {
                continue;
            }
            let source = match memory.u32_at(address + SOURCE) {
                0 => SOURCE_TITLE,
                source => source,
            };
            let bytes = if binary(id) {
                let (length, data) = (memory.u32_at(address + VALUE), memory.u32_at(address + DATA));
                if data == 0 || length == 0 || length > LARGEST_SETTING {
                    continue;
                }
                memory.bytes(data, length as usize)
            } else {
                memory.bytes(address + VALUE, 8)
            };
            stored.insert(id, Setting { source, bytes });
            kept += 1;
        }
    }

    if kept != 0 {
        save(profile);
    }
    log_kernel!("profile: %u of %u settings written to %s", kept, count, file_text(profile).as_c_str());
    drop(profiles);
    complete_overlapped(memory, overlapped, ERROR_SUCCESS);
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});
