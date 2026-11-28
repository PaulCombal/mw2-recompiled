// Starting another program beside the launcher and leaving it running: the game, or the
// launcher an update installed. It gets a copy of the launcher's environment with a few
// variables set, which never reach the launcher's own, so a game started the plain way
// later does not have them. It gets the launcher's own standard streams where it has
// them. A Windows program opened from the desktop has none, and SDL fails the start when
// asked to pass on a stream that is not there ("DuplicateHandle(): The handle is
// invalid"), so those are left unconnected instead.
#![allow(unsafe_code)]

use sdl3_sys::everything as sdl;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::Path;
use std::ptr;

#[cfg(windows)]
mod streams {
    use std::ffi::c_void;

    const FILE_TYPE_UNKNOWN: u32 = 0;
    const NO_ERROR: u32 = 0;
    const INVALID_HANDLE_VALUE: *mut c_void = -1isize as *mut c_void;

    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn SetLastError(error: u32);
        fn GetLastError() -> u32;
        fn GetFileType(handle: *mut c_void) -> u32;
    }

    pub const INPUT: u32 = -10i32 as u32;
    pub const OUTPUT: u32 = -11i32 as u32;
    pub const ERROR: u32 = -12i32 as u32;

    // Whether the launcher has this standard stream to hand on.
    pub fn has(which: u32) -> bool {
        // SAFETY: plain calls on the process's own handles.
        unsafe {
            let handle = GetStdHandle(which);
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return false;
            }
            SetLastError(NO_ERROR);
            GetFileType(handle) != FILE_TYPE_UNKNOWN || GetLastError() == NO_ERROR
        }
    }
}

/// What SDL says went wrong last.
pub fn last_error() -> String {
    // SAFETY: SDL's own string.
    unsafe { CStr::from_ptr(sdl::SDL_GetError()) }.to_string_lossy().into_owned()
}

/// A thread of the launcher's own, for a job (thread_builder's).
pub fn thread<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> std::thread::JoinHandle<T> {
    thread_builder().spawn(body).unwrap_or_else(|e| panic!("a thread for the launcher's job: {e}"))
}

/// How the launcher's threads are made: on Linux with glibc's default stack, which
/// RLIMIT_STACK sets (8 MiB as a rule), where Rust's default is 2 MiB -- the map converter
/// the jobs call needs it, and the shader compile pass's threads translate on it; on
/// Windows with Rust's.
pub fn thread_builder() -> std::thread::Builder {
    let builder = std::thread::Builder::new();
    match default_stack() {
        Some(size) => builder.stack_size(size),
        None => builder,
    }
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
#[allow(unsafe_code)]
fn default_stack() -> Option<usize> {
    use std::ffi::c_int;
    // pthread_attr_t, which glibc makes 56 bytes on x86-64.
    #[repr(C, align(8))]
    struct Attributes([u8; 64]);
    unsafe extern "C" {
        fn pthread_attr_init(attributes: *mut Attributes) -> c_int;
        fn pthread_attr_getstacksize(attributes: *const Attributes, size: *mut usize) -> c_int;
        fn pthread_attr_destroy(attributes: *mut Attributes) -> c_int;
    }
    let mut attributes = Attributes([0; 64]);
    let mut size = 0;
    // SAFETY: an attribute set made, read and destroyed here; one with no size of its own
    // gives the default.
    unsafe {
        if pthread_attr_init(&mut attributes) != 0 {
            return None;
        }
        let read = pthread_attr_getstacksize(&attributes, &mut size) == 0;
        pthread_attr_destroy(&mut attributes);
        read.then_some(size)
    }
}

#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
fn default_stack() -> Option<usize> {
    None
}

/// The program beside the launcher, by its absolute path, started with these variables
/// set; SDL's words when it cannot be.
pub fn start(program: &Path, set: &[(&str, &[u8])]) -> Result<(), String> {
    let path = std::path::absolute(program).unwrap_or_else(|_| program.to_path_buf());
    let path = CString::new(crate::boundary::utf8(&path)).map_err(|_| "a zero byte in the path".to_string())?;
    let mut args: [*const c_char; 2] = [path.as_ptr(), ptr::null()];
    // SAFETY: SDL's calls, on what they are given: C strings alive across them.
    unsafe {
        let environment = sdl::SDL_CreateEnvironment(true);
        for (name, value) in set {
            let (name, value) = (CString::new(*name).unwrap_or_default(), CString::new(*value).unwrap_or_default());
            sdl::SDL_SetEnvironmentVariable(environment, name.as_ptr(), value.as_ptr(), true);
        }
        let properties = sdl::SDL_CreateProperties();
        sdl::SDL_SetPointerProperty(
            properties,
            sdl::SDL_PROP_PROCESS_CREATE_ARGS_POINTER,
            args.as_mut_ptr().cast::<c_void>(),
        );
        sdl::SDL_SetPointerProperty(properties, sdl::SDL_PROP_PROCESS_CREATE_ENVIRONMENT_POINTER, environment.cast());
        #[cfg(windows)]
        for (stream, which) in [
            (sdl::SDL_PROP_PROCESS_CREATE_STDIN_NUMBER, streams::INPUT),
            (sdl::SDL_PROP_PROCESS_CREATE_STDOUT_NUMBER, streams::OUTPUT),
            (sdl::SDL_PROP_PROCESS_CREATE_STDERR_NUMBER, streams::ERROR),
        ] {
            let io = if streams::has(which) { sdl::SDL_PROCESS_STDIO_INHERITED } else { sdl::SDL_PROCESS_STDIO_NULL };
            sdl::SDL_SetNumberProperty(properties, stream, io.0 as i64);
        }
        let process = sdl::SDL_CreateProcessWithProperties(properties);
        sdl::SDL_DestroyProperties(properties);
        sdl::SDL_DestroyEnvironment(environment);
        if process.is_null() {
            return Err(last_error());
        }
        sdl::SDL_DestroyProcess(process); // forgets it; it keeps running
    }
    Ok(())
}
