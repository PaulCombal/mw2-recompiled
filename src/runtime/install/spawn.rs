// What a start asks of SDL: a message for the player, and starting another program,
// the launcher or the other title.

use sdl3_sys::everything as sdl;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::Path;
use std::ptr;

/// A path as UTF-8, which SDL and the log take, whatever the system's code page.
pub fn c_path(path: &Path) -> CString {
    #[cfg(not(windows))]
    {
        use std::os::unix::ffi::OsStrExt;
        CString::new(path.as_os_str().as_bytes()).unwrap_or_default()
    }
    #[cfg(windows)]
    {
        CString::new(path.to_string_lossy().as_bytes()).unwrap_or_default()
    }
}

pub fn message_box(text: &CStr) {
    unsafe {
        sdl::SDL_ShowSimpleMessageBox(
            sdl::SDL_MESSAGEBOX_ERROR,
            c"Modern Warfare 2".as_ptr(),
            text.as_ptr(),
            ptr::null_mut(),
        )
    };
}

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

    // Whether this program has this standard stream to hand on.
    pub fn has(which: u32) -> bool {
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

#[cfg(windows)]
fn connect(properties: sdl::SDL_PropertiesID, stream: *const c_char, has: bool) {
    let io = if has { sdl::SDL_PROCESS_STDIO_INHERITED } else { sdl::SDL_PROCESS_STDIO_NULL };
    unsafe { sdl::SDL_SetNumberProperty(properties, stream, io.0 as i64) };
}

// What SDL says went wrong last.
fn last_error() -> String {
    unsafe { CStr::from_ptr(sdl::SDL_GetError()) }.to_string_lossy().into_owned()
}

/// Starts `arguments`, the program first, and forgets it: it keeps running. With
/// `changes`, the environment it gets is this one's with those set, or unset where
/// there is no value. It gets this one's own standard streams where it has them. A
/// Windows program opened from the desktop has none, and SDL fails the start when asked
/// to pass on a stream that is not there ("DuplicateHandle(): The handle is invalid"),
/// so those are left unconnected instead.
pub fn start(arguments: &[&CStr], changes: Option<&[(&CStr, Option<&CStr>)]>) -> Result<(), String> {
    let mut args: Vec<*const c_char> = arguments.iter().map(|argument| argument.as_ptr()).collect();
    args.push(ptr::null());
    unsafe {
        let properties = sdl::SDL_CreateProperties();
        sdl::SDL_SetPointerProperty(
            properties,
            sdl::SDL_PROP_PROCESS_CREATE_ARGS_POINTER,
            args.as_mut_ptr().cast::<c_void>(),
        );
        if let Some(changes) = changes {
            let environment = sdl::SDL_GetEnvironment();
            for (name, value) in changes {
                match value {
                    Some(value) => sdl::SDL_SetEnvironmentVariable(environment, name.as_ptr(), value.as_ptr(), true),
                    None => sdl::SDL_UnsetEnvironmentVariable(environment, name.as_ptr()),
                };
            }
            sdl::SDL_SetPointerProperty(
                properties,
                sdl::SDL_PROP_PROCESS_CREATE_ENVIRONMENT_POINTER,
                environment.cast(),
            );
        }
        #[cfg(windows)]
        {
            connect(properties, sdl::SDL_PROP_PROCESS_CREATE_STDIN_NUMBER, streams::has(streams::INPUT));
            connect(properties, sdl::SDL_PROP_PROCESS_CREATE_STDOUT_NUMBER, streams::has(streams::OUTPUT));
            connect(properties, sdl::SDL_PROP_PROCESS_CREATE_STDERR_NUMBER, streams::has(streams::ERROR));
        }
        let process = sdl::SDL_CreateProcessWithProperties(properties);
        sdl::SDL_DestroyProperties(properties);
        if process.is_null() {
            return Err(last_error());
        }
        sdl::SDL_DestroyProcess(process); // forgets it; it keeps running
    }
    Ok(())
}
