// Paths as the launcher's text is: bytes, on Linux a path's own, on Windows its whole name
// in UTF-8.

use std::path::{Path, PathBuf};

/// The path of bytes as utf8 gives them.
pub fn path_of(text: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(text))
    }
    #[cfg(not(unix))]
    PathBuf::from(String::from_utf8_lossy(text).into_owned())
}

/// A path's bytes: its own on Linux, its name in UTF-8 on Windows.
pub fn utf8(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    path.to_string_lossy().into_owned().into_bytes()
}
