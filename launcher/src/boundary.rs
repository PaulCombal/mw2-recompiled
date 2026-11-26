// What crosses between the Rust and the C++ that stays (maps.cpp): text
// as bytes and a length, and paths as UTF-8, which the C++ makes a path of with
// install::FromUtf8 -- on Linux a path's own bytes, on Windows its whole name.
#![allow(unsafe_code)]

use std::ffi::c_char;
use std::path::{Path, PathBuf};

/// The bytes the C++ handed over.
///
/// # Safety
/// `text` points at `length` readable bytes, or is null with a length of 0, and they stay
/// as they are while the slice is used.
pub unsafe fn bytes<'a>(text: *const c_char, length: usize) -> &'a [u8] {
    if text.is_null() || length == 0 { &[] } else { unsafe { std::slice::from_raw_parts(text.cast(), length) } }
}

/// A path the C++ handed over as UTF-8.
pub fn path_of(text: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(text))
    }
    #[cfg(not(unix))]
    PathBuf::from(String::from_utf8_lossy(text).into_owned())
}

/// A path as UTF-8 for the C++, as install::Utf8 spells it.
pub fn utf8(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    path.to_string_lossy().into_owned().into_bytes()
}
