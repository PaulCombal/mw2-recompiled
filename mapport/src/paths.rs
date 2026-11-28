// Paths as the converter's text (crate::text): a path's bytes, a char each, as its names
// and messages carry them -- on Linux a path's own bytes, on Windows its name in UTF-8.

use crate::text;
use std::path::{Path, PathBuf};

fn bytes_of(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    path.to_string_lossy().into_owned().into_bytes()
}

/// A path as text.
pub fn utf8(path: &Path) -> String {
    text::of(&bytes_of(path))
}

/// The path text of `utf8` names.
pub fn path_of(name: &str) -> PathBuf {
    let bytes = text::bytes(name);
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(&bytes))
    }
    #[cfg(not(unix))]
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

/// A path's last part as text, or nothing.
pub fn file_name(path: &Path) -> String {
    path.file_name().map(|n| utf8(Path::new(n))).unwrap_or_default()
}
