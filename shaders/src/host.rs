// What this crate asks of the program around it, and of the C library.
//
// The log: a line goes to the sink the program sets (the game's log, with its stamp and
// its MW2_LOGGING switch), else whole to the C library's stderr. The switches are read
// through the C library's getenv, as the game's are (src/env.rs): the values the game's
// .env file gave are in its environment. The pipeline cache's file is read and written
// with stdio, so that a path reads as the game's other files do on Windows (the ANSI code
// page), and a failed rename is named in the C++ library's words, which the game and the
// launcher have always logged.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::RwLock;

unsafe extern "C" {
    fn getenv(name: *const c_char) -> *const c_char;
    fn fputs(text: *const c_char, stream: *mut c_void) -> c_int;
    #[cfg(not(windows))]
    #[link_name = "stderr"]
    static STDERR: *mut c_void;
    #[cfg(windows)]
    fn __acrt_iob_func(index: u32) -> *mut c_void;
}

type Sink = fn(line: &[u8]);

static SINK: RwLock<Option<Sink>> = RwLock::new(None);

/// Where this crate's log lines go: `sink` is handed each one whole, its level's prefix
/// ("[i] ", "[w] ", "[E] ") first and no line end.
pub fn set_log(sink: Sink) {
    *SINK.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(sink);
}

pub(crate) fn log(line: &[u8]) {
    let sink = *SINK.read().unwrap_or_else(|poisoned| poisoned.into_inner());
    match sink {
        Some(sink) => sink(line),
        None => {
            let mut text: Vec<u8> = line.iter().copied().filter(|&byte| byte != 0).collect();
            text.extend_from_slice(b"\n\0");
            #[cfg(not(windows))]
            let stream = unsafe { STDERR };
            #[cfg(windows)]
            let stream = unsafe { __acrt_iob_func(2) };
            // SAFETY: a C string, and the C library's stderr.
            unsafe { fputs(text.as_ptr().cast(), stream) };
        }
    }
}

/// A log line from its parts, bytes as they are: a device's name or a path is written
/// as printf's %s writes it.
pub(crate) fn log_parts(parts: &[&[u8]]) {
    log(&parts.concat());
}

/// A log line from its prefix and a text made with write!.
macro_rules! log_line {
    ($prefix:literal, $($format:tt)*) => {{
        let mut line: Vec<u8> = $prefix.to_vec();
        {
            use std::io::Write as _;
            let _ = write!(line, $($format)*);
        }
        $crate::host::log(&line);
    }};
}
pub(crate) use log_line;

/// runtime/env.h's Text: null when unset or empty.
pub(crate) fn text(name: &CStr) -> Option<&'static CStr> {
    // SAFETY: a C string; getenv's answer stays while the environment is not changed,
    // which nothing does once a run has started.
    let value = unsafe { getenv(name.as_ptr()) };
    if value.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(value) }).filter(|value| !value.is_empty())
}

/// runtime/env.h's Flag: on when set to anything but "" or "0".
pub(crate) fn flag(name: &CStr) -> bool {
    text(name).is_some_and(|value| value != c"0")
}

/// The C library's file calls, for the pipeline cache's file.
pub(crate) mod stdio {
    use std::ffi::{CStr, c_char, c_int, c_long, c_void};

    unsafe extern "C" {
        fn fopen(path: *const c_char, mode: *const c_char) -> *mut c_void;
        fn fclose(file: *mut c_void) -> c_int;
        fn fseek(file: *mut c_void, offset: c_long, whence: c_int) -> c_int;
        fn ftell(file: *mut c_void) -> c_long;
        fn fread(buffer: *mut c_void, size: usize, count: usize, file: *mut c_void) -> usize;
        fn fwrite(buffer: *const c_void, size: usize, count: usize, file: *mut c_void) -> usize;
    }

    const SEEK_SET: c_int = 0;
    const SEEK_END: c_int = 2;

    /// The whole file: nothing when it does not open, is empty (by ftell), or reads
    /// short.
    pub fn read(path: &CStr) -> Vec<u8> {
        let mut data = Vec::new();
        // SAFETY: C strings, and a FILE the calls below own until fclose.
        unsafe {
            let file = fopen(path.as_ptr(), c"rb".as_ptr());
            if file.is_null() {
                return data;
            }
            fseek(file, 0, SEEK_END);
            let size = ftell(file);
            fseek(file, 0, SEEK_SET);
            if size > 0 {
                data.resize(size as usize, 0);
                if fread(data.as_mut_ptr().cast(), 1, data.len(), file) != data.len() {
                    data.clear();
                }
            }
            fclose(file);
        }
        data
    }

    /// Writes `data`: false if it does not open, writes short, or does
    /// not close.
    pub fn write(path: &CStr, data: &[u8]) -> bool {
        // SAFETY: C strings, `data` readable, and a FILE owned until fclose.
        unsafe {
            let file = fopen(path.as_ptr(), c"wb".as_ptr());
            if file.is_null() {
                return false;
            }
            let written = fwrite(data.as_ptr().cast(), 1, data.len(), file) == data.len();
            fclose(file) == 0 && written
        }
    }
}

/// std::filesystem's rename and remove on a path std::string holds: rename with its
/// error_code's message() -- the C library's text for the errno (libstdc++), or the
/// system's for the Windows error (libc++) --, remove with its error ignored.
pub mod files {
    use std::ffi::CStr;
    #[cfg(not(windows))]
    use std::ffi::{c_char, c_int};

    #[cfg(not(windows))]
    unsafe extern "C" {
        fn rename(from: *const c_char, to: *const c_char) -> c_int;
        fn remove(path: *const c_char) -> c_int;
        fn __errno_location() -> *mut c_int;
    }

    #[cfg(not(windows))]
    unsafe extern "C" {
        fn strerror(error: c_int) -> *const c_char;
    }

    #[cfg(not(windows))]
    fn message(error: c_int) -> Vec<u8> {
        // SAFETY: strerror's text, copied at once.
        unsafe { CStr::from_ptr(strerror(error)) }.to_bytes().to_vec()
    }

    /// libstdc++'s rename: ::rename, the errno's text on failure.
    #[cfg(not(windows))]
    pub fn rename_file(from: &CStr, to: &CStr) -> Result<(), Vec<u8>> {
        // SAFETY: C strings.
        if unsafe { rename(from.as_ptr(), to.as_ptr()) } == 0 {
            return Ok(());
        }
        Err(message(unsafe { *__errno_location() }))
    }

    /// libstdc++'s remove: the C library's, which takes a file, or an empty folder of
    /// that name.
    #[cfg(not(windows))]
    pub fn remove_file(path: &CStr) {
        // SAFETY: a C string.
        unsafe { remove(path.as_ptr()) };
    }

    #[cfg(windows)]
    mod win32 {
        use std::ffi::c_char;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            pub fn MultiByteToWideChar(
                code_page: u32,
                flags: u32,
                text: *const c_char,
                length: i32,
                wide: *mut u16,
                wide_length: i32,
            ) -> i32;
            pub fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
            pub fn CreateFileW(
                path: *const u16,
                access: u32,
                share: u32,
                security: *const std::ffi::c_void,
                disposition: u32,
                flags: u32,
                template: *mut std::ffi::c_void,
            ) -> *mut std::ffi::c_void;
            pub fn SetFileInformationByHandle(
                file: *mut std::ffi::c_void,
                class: u32,
                information: *const std::ffi::c_void,
                size: u32,
            ) -> i32;
            pub fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
            pub fn GetLastError() -> u32;
            pub fn AreFileApisANSI() -> i32;
            pub fn FormatMessageA(
                flags: u32,
                source: *const std::ffi::c_void,
                message: u32,
                language: u32,
                buffer: *mut c_char,
                size: u32,
                arguments: *const std::ffi::c_void,
            ) -> u32;
            pub fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        }
        pub const FORMAT_MESSAGE_ALLOCATE_BUFFER: u32 = 0x100;
        pub const FORMAT_MESSAGE_FROM_SYSTEM: u32 = 0x1000;
        pub const FORMAT_MESSAGE_IGNORE_INSERTS: u32 = 0x200;
        pub const CP_ACP: u32 = 0;
        pub const CP_OEMCP: u32 = 1;
        pub const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
        pub const MOVEFILE_COPY_ALLOWED: u32 = 0x2;
        pub const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
        pub const DELETE: u32 = 0x10000;
        pub const FILE_SHARE_READ_WRITE_DELETE: u32 = 0x7;
        pub const OPEN_EXISTING: u32 = 3;
        pub const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        pub const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        pub const FILE_DISPOSITION_INFO: u32 = 4;
        pub const INVALID_HANDLE_VALUE: *mut std::ffi::c_void = usize::MAX as *mut std::ffi::c_void;
    }

    // A narrow path as libc++'s std::filesystem::path takes it on Windows: in the code
    // page the file functions use.
    #[cfg(windows)]
    fn wide(path: &CStr) -> Vec<u16> {
        use win32::*;
        // SAFETY: a C string, and a buffer of the size the first call asks for.
        unsafe {
            let page = if AreFileApisANSI() != 0 { CP_ACP } else { CP_OEMCP };
            let length = MultiByteToWideChar(page, 0, path.as_ptr(), -1, std::ptr::null_mut(), 0);
            let mut text = vec![0u16; length.max(1) as usize];
            MultiByteToWideChar(page, 0, path.as_ptr(), -1, text.as_mut_ptr(), length);
            text
        }
    }

    // libc++'s system_category().message() on Windows (__system_error_category::message
    // in system_error.cpp): FormatMessageA from the system in the default language (0),
    // the buffer allocated by the call, trailing spaces, carriage returns and line feeds
    // cut, and "Unknown error" when the lookup fails or nothing is left.
    #[cfg(windows)]
    fn system_message(error: u32) -> Vec<u8> {
        use std::ffi::c_char;
        use win32::*;
        let mut buffer: *mut c_char = std::ptr::null_mut();
        // SAFETY: with FORMAT_MESSAGE_ALLOCATE_BUFFER the buffer argument receives a
        // LocalAlloc'd pointer to `length` bytes, freed below.
        let length = unsafe {
            FormatMessageA(
                FORMAT_MESSAGE_ALLOCATE_BUFFER | FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
                std::ptr::null(),
                error,
                0,
                (&raw mut buffer).cast(),
                0,
                std::ptr::null(),
            )
        } as usize;
        let mut text = if length == 0 || buffer.is_null() {
            Vec::new()
        } else {
            // SAFETY: the `length` bytes FormatMessageA wrote.
            unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), length) }.to_vec()
        };
        // SAFETY: the buffer FormatMessageA allocated, or null.
        unsafe { LocalFree(buffer.cast()) };
        while text.last().is_some_and(|byte| matches!(byte, b' ' | b'\r' | b'\n')) {
            text.pop();
        }
        if text.is_empty() {
            return b"Unknown error".to_vec();
        }
        text
    }

    /// libc++'s rename on Windows (posix_compat.h's detail::rename): MoveFileExW
    /// with MOVEFILE_COPY_ALLOWED, MOVEFILE_REPLACE_EXISTING and MOVEFILE_WRITE_THROUGH,
    /// the error GetLastError's in system_category.
    #[cfg(windows)]
    pub fn rename_file(from: &CStr, to: &CStr) -> Result<(), Vec<u8>> {
        use win32::*;
        let (from, to) = (wide(from), wide(to));
        let flags = MOVEFILE_COPY_ALLOWED | MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;
        // SAFETY: two wide C strings.
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) } != 0 {
            return Ok(());
        }
        Err(system_message(unsafe { GetLastError() }))
    }

    /// libc++'s remove on Windows (posix_compat.h's detail::remove): the file or folder
    /// opened for DELETE, itself rather than what a link names, then marked to be deleted
    /// when closed (FileDispositionInfo). Unlike DeleteFileW and Rust's fs::remove_file,
    /// that refuses a read-only file, which is then left as it is.
    #[cfg(windows)]
    pub fn remove_file(path: &CStr) {
        use win32::*;
        let path = wide(path);
        // SAFETY: a wide C string, and the handle it opens, closed below.
        unsafe {
            let file = CreateFileW(
                path.as_ptr(),
                DELETE,
                FILE_SHARE_READ_WRITE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            );
            if file == INVALID_HANDLE_VALUE {
                return;
            }
            // FILE_DISPOSITION_INFO: a BOOLEAN DeleteFile.
            let delete: u8 = 1;
            SetFileInformationByHandle(file, FILE_DISPOSITION_INFO, (&raw const delete).cast(), 1);
            CloseHandle(file);
        }
    }
}
