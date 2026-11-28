// One launcher window a copy of the game: a second start of the window brings the first
// one up in front and ends. The copy is the launcher's folder, so two installs side by
// side each have their own.
//
// Linux has a socket in the abstract namespace named after the folder, which ends with
// the process that holds it and leaves no file; a connection to it is the message.
// Windows has a named mutex and an event the second start sets. A window brought up is
// one the system allows in front: Windows lets the second start hand its place over, X11
// raises it, and a Wayland compositor may only mark it as asking for attention.
#![deny(unsafe_code)]

use std::time::Duration;

// How long the launcher an update started waits for the old one to end: it ends as soon
// as it has started the new one.
const TAKE_OVER: Duration = Duration::from_secs(5);

// The copy's name: its folder as SDL names it, hashed (FNV-1a), lower case on Windows.
#[allow(unsafe_code)]
fn name() -> String {
    use sdl3_sys::everything as sdl;
    // SAFETY: SDL's own string, or null.
    let base = unsafe { sdl::SDL_GetBasePath() };
    let mut folder =
        if base.is_null() { Vec::new() } else { unsafe { std::ffi::CStr::from_ptr(base) }.to_bytes().to_vec() };
    if cfg!(windows) {
        folder.make_ascii_lowercase();
    }
    let hash = folder
        .iter()
        .fold(0xcbf2_9ce4_8422_2325u64, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3));
    format!("mw2-launcher-{hash:016x}")
}

#[cfg(target_os = "linux")]
pub mod system {
    use super::{TAKE_OVER, name};
    use std::io::ErrorKind;
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    static HELD: Mutex<Option<UnixListener>> = Mutex::new(None);

    fn address() -> Option<SocketAddr> {
        SocketAddr::from_abstract_name(name().as_bytes()).ok()
    }

    pub fn claim(take_over: bool) -> bool {
        let Some(address) = address() else { return false };
        let until = Instant::now() + TAKE_OVER;
        loop {
            if let Ok(listener) = UnixListener::bind_addr(&address) {
                if listener.set_nonblocking(true).is_err() {
                    return false;
                }
                *HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(listener);
                return true;
            }
            if !take_over || Instant::now() > until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn wake() {
        if let Some(address) = address() {
            let _ = UnixStream::connect_addr(&address);
        }
    }

    pub fn woken() -> bool {
        let held = HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(listener) = held.as_ref() else { return false };
        let mut woken = false;
        loop {
            match listener.accept() {
                Ok(_) => woken = true,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => return woken,
            }
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub mod system {
    use super::{TAKE_OVER, name};
    use std::ffi::{CString, c_void};
    use std::ptr;
    use std::sync::Mutex;

    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_ABANDONED: u32 = 0x80;
    const ASFW_ANY: u32 = u32::MAX;

    // kernel32 is always linked; AllowSetForegroundWindow is user32's.
    #[link(name = "user32")]
    unsafe extern "system" {
        fn CreateEventA(attributes: *mut c_void, manual: i32, initial: i32, name: *const u8) -> *mut c_void;
        fn CreateMutexA(attributes: *mut c_void, owner: i32, name: *const u8) -> *mut c_void;
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        fn SetEvent(handle: *mut c_void) -> i32;
        fn AllowSetForegroundWindow(process: u32) -> i32;
    }

    // The handles, kept for the process's life, as numbers.
    struct Held {
        mutex: usize,
        wake: usize,
        claimed: bool,
    }
    static HELD: Mutex<Held> = Mutex::new(Held { mutex: 0, wake: 0, claimed: false });

    fn wakeup() -> usize {
        let name = CString::new(format!("Local\\{}-wake", name())).unwrap_or_default();
        // SAFETY: a C string; no attributes.
        unsafe { CreateEventA(ptr::null_mut(), 0, 0, name.as_ptr().cast()) as usize }
    }

    pub fn claim(take_over: bool) -> bool {
        let mut held = HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let name = CString::new(format!("Local\\{}", name())).unwrap_or_default();
        held.wake = wakeup();
        // SAFETY: a C string; no attributes.
        held.mutex = unsafe { CreateMutexA(ptr::null_mut(), 0, name.as_ptr().cast()) as usize };
        if held.mutex == 0 {
            return false;
        }
        let wait = if take_over { TAKE_OVER.as_millis() as u32 } else { 0 };
        // SAFETY: the mutex just opened.
        let waited = unsafe { WaitForSingleObject(held.mutex as *mut c_void, wait) };
        held.claimed = waited == WAIT_OBJECT_0 || waited == WAIT_ABANDONED;
        held.claimed
    }

    pub fn wake() {
        // Only the program in front may put another there, and a start the player made
        // is: it hands the place over.
        // SAFETY: plain calls; the event is ours.
        unsafe { AllowSetForegroundWindow(ASFW_ANY) };
        let mut held = HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if held.wake == 0 {
            held.wake = wakeup();
        }
        if held.wake != 0 {
            unsafe { SetEvent(held.wake as *mut c_void) };
        }
    }

    pub fn woken() -> bool {
        let held = HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // SAFETY: the event opened at the claim.
        held.claimed && held.wake != 0 && unsafe { WaitForSingleObject(held.wake as *mut c_void, 0) } == WAIT_OBJECT_0
    }
}
