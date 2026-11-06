// The runtime's log, from Rust: log_info!("fov: %g", value).
//
// A line is written whole, with one fprintf, to the C library's stderr: the one the
// C++ renderer's lines go to (runtime/log.h), which MW2_LOG_FILE moved and which is
// the only one a Windows player's build has. eprintln! would write past it, and
// panics when a write fails. printf does the formatting, so a line reads the same as
// the C++ it replaced; a format that does not match its arguments does not compile.
// MW2_LOG_TIME's stamp is the same clock as the C++'s, which asks for it here, and a
// build without MW2_LOGGING (MW2_LOGGING=OFF in the environment, which build.rs reads) writes no line.

use crate::guest::GuestText;
use std::ffi::{CStr, CString, c_char, c_int, c_longlong, c_uint, c_ulonglong, c_void};
use std::sync::atomic::{AtomicU32, Ordering};

unsafe extern "C" {
    fn snprintf(buffer: *mut c_char, size: usize, format: *const c_char, ...) -> c_int;
    fn fprintf(stream: *mut c_void, format: *const c_char, ...) -> c_int;
    #[cfg(not(windows))]
    #[link_name = "stderr"]
    static STDERR: *mut c_void;
    #[cfg(windows)]
    fn __acrt_iob_func(index: u32) -> *mut c_void;
}

// The C library's stderr, as it stands: freopen keeps the same stream.
fn standard_error() -> *mut c_void {
    #[cfg(not(windows))]
    return unsafe { STDERR };
    #[cfg(windows)]
    return unsafe { __acrt_iob_func(2) };
}

/// MW2_LOG_TIME=1 starts every line with the seconds since the first stamped one, so
/// a line can be matched with something seen on screen at that moment. For this
/// thread, until its next line: runtime/log.h's Stamp() in the Rust build.
pub fn stamp() -> *const c_char {
    #[cfg(feature = "diagnostics")]
    {
        use std::cell::UnsafeCell;
        use std::sync::{LazyLock, OnceLock};
        use std::time::Instant;
        static ON: LazyLock<bool> = LazyLock::new(|| crate::env::flag(c"MW2_LOG_TIME"));
        static START: OnceLock<Instant> = OnceLock::new();
        thread_local! {
            static TEXT: UnsafeCell<[c_char; 24]> = const { UnsafeCell::new([0; 24]) };
        }
        if *ON {
            let seconds = START.get_or_init(Instant::now).elapsed().as_secs_f64();
            return TEXT.with(|text| {
                let text = text.get().cast::<c_char>();
                unsafe { snprintf(text, 24, c"%9.3f ".as_ptr(), seconds) };
                text.cast_const()
            });
        }
    }
    c"".as_ptr()
}

/// One line, whole, after the stamp.
fn write_line(text: &CStr) {
    if cfg!(mw2_no_logging) {
        return;
    }
    unsafe { fprintf(standard_error(), c"%s%s\n".as_ptr(), stamp(), text.as_ptr()) };
}

// What printf makes of a line's arguments, `write` being the snprintf, then the line.
#[inline(never)]
fn print_with(write: &mut dyn FnMut(*mut c_char, usize) -> c_int) {
    let mut buffer: [c_char; 512] = [0; 512];
    let wanted = write(buffer.as_mut_ptr(), buffer.len()).max(0) as usize;
    if wanted < buffer.len() {
        write_line(unsafe { CStr::from_ptr(buffer.as_ptr()) });
        return;
    }
    let mut text = vec![0 as c_char; wanted + 1];
    write(text.as_mut_ptr(), text.len());
    write_line(unsafe { CStr::from_ptr(text.as_ptr()) });
}

#[derive(Copy, Clone)]
pub enum Kind {
    Real,
    Text,
    Number,
    Signed,
    LongSigned,
    Unsigned,
    Pointer,
}

pub trait Argument: Copy {
    const KIND: Kind;
    type Passed;
    fn passed(self) -> Self::Passed;
}

impl Argument for f64 {
    const KIND: Kind = Kind::Real;
    type Passed = f64;
    fn passed(self) -> f64 {
        self
    }
}

impl Argument for u64 {
    const KIND: Kind = Kind::Number;
    type Passed = c_ulonglong;
    fn passed(self) -> c_ulonglong {
        self
    }
}

impl Argument for i64 {
    const KIND: Kind = Kind::LongSigned;
    type Passed = c_longlong;
    fn passed(self) -> c_longlong {
        self
    }
}

impl Argument for i32 {
    const KIND: Kind = Kind::Signed;
    type Passed = c_int;
    fn passed(self) -> c_int {
        self
    }
}

impl Argument for u32 {
    const KIND: Kind = Kind::Unsigned;
    type Passed = c_uint;
    fn passed(self) -> c_uint {
        self
    }
}

impl Argument for *const c_void {
    const KIND: Kind = Kind::Pointer;
    type Passed = *const c_void;
    fn passed(self) -> *const c_void {
        self
    }
}

impl Argument for &CStr {
    const KIND: Kind = Kind::Text;
    type Passed = *const c_char;
    fn passed(self) -> *const c_char {
        self.as_ptr()
    }
}

impl Argument for GuestText {
    const KIND: Kind = Kind::Text;
    type Passed = *const c_char;
    fn passed(self) -> *const c_char {
        self.0
    }
}

pub trait Format {
    const TEXT: &'static CStr;
}

pub trait Arguments: Copy {
    const KINDS: &'static [Kind];
    fn print(self, format: &CStr);
    fn write(self, format: &CStr, buffer: *mut c_char, size: usize) -> c_int;
}

macro_rules! arguments {
    ($($name:ident $index:tt),*) => {
        impl<$($name: Argument),*> Arguments for ($($name,)*) {
            const KINDS: &'static [Kind] = &[$($name::KIND),*];
            #[inline(always)]
            fn print(self, format: &CStr) {
                print_with(&mut |buffer, size| self.write(format, buffer, size));
            }
            fn write(self, format: &CStr, buffer: *mut c_char, size: usize) -> c_int {
                unsafe { snprintf(buffer, size, format.as_ptr(), $(self.$index.passed()),*) }
            }
        }
    };
}
arguments!();
arguments!(A 0);
arguments!(A 0, B 1);
arguments!(A 0, B 1, C 2);
arguments!(A 0, B 1, C 2, D 3);
arguments!(A 0, B 1, C 2, D 3, E 4);
arguments!(A 0, B 1, C 2, D 3, E 4, F 5);
arguments!(A 0, B 1, C 2, D 3, E 4, F 5, G 6);
arguments!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7);
arguments!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8);
arguments!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9);
arguments!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10);
arguments!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10, L 11);

// %g, %f, %s, %d, %u, %x, %X, %p with printf's flags, width and precision, %llu, %llx
// and %llX for a u64, %lld for an i64, and %%: each for its types, and as many as there are arguments.
const fn agrees(format: &[u8], kinds: &[Kind]) -> bool {
    let (mut at, mut used) = (0, 0);
    while at < format.len() {
        if format[at] != b'%' {
            at += 1;
            continue;
        }
        at += 1;
        if at < format.len() && format[at] == b'%' {
            at += 1;
            continue;
        }
        while at < format.len() && matches!(format[at], b'-' | b'+' | b' ' | b'#' | b'0') {
            at += 1;
        }
        while at < format.len() && format[at].is_ascii_digit() {
            at += 1;
        }
        if at < format.len() && format[at] == b'.' {
            at += 1;
            while at < format.len() && format[at].is_ascii_digit() {
                at += 1;
            }
        }
        let long = at + 1 < format.len() && format[at] == b'l' && format[at + 1] == b'l';
        if long {
            at += 2;
        }
        if at == format.len() {
            return false;
        }
        let kind = match (format[at], long) {
            (b'g' | b'f', false) => Kind::Real,
            (b's', false) => Kind::Text,
            (b'd', false) => Kind::Signed,
            (b'd', true) => Kind::LongSigned,
            (b'u' | b'x' | b'X', false) => Kind::Unsigned,
            (b'u' | b'x' | b'X', true) => Kind::Number,
            (b'p', false) => Kind::Pointer,
            _ => return false,
        };
        if used == kinds.len() || kind as u8 != kinds[used] as u8 {
            return false;
        }
        used += 1;
        at += 1;
    }
    used == kinds.len()
}

/// Counts what a peer can repeat at will, and gives the count back at every power
/// of two, so that a flood stays a few lines.
#[derive(Default)]
pub struct Doubling(AtomicU32);

impl Doubling {
    pub const fn new() -> Self {
        Self(AtomicU32::new(0))
    }

    pub fn count(&self) -> Option<u32> {
        let n = self.0.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
        (n & n.wrapping_sub(1) == 0).then_some(n)
    }
}

#[inline(always)]
pub fn line<F: Format, A: Arguments>(arguments: A) {
    const { assert!(agrees(F::TEXT.to_bytes(), A::KINDS), "a log line's format does not match its arguments") };
    arguments.print(F::TEXT);
}

/// What printf makes of the arguments, for a line of a file or a part of a log line.
pub fn formatted<F: Format, A: Arguments>(arguments: A) -> CString {
    const { assert!(agrees(F::TEXT.to_bytes(), A::KINDS), "a text's format does not match its arguments") };
    let mut text = vec![0u8; 256];
    let wanted = arguments.write(F::TEXT, text.as_mut_ptr().cast(), text.len()).max(0) as usize;
    if wanted >= text.len() {
        text.resize(wanted + 1, 0);
        arguments.write(F::TEXT, text.as_mut_ptr().cast(), text.len());
    }
    text.truncate(wanted);
    c_string(text)
}

/// `text` up to its first zero, as a C string holding it would read.
pub fn c_string(text: impl Into<Vec<u8>>) -> CString {
    let mut text = text.into();
    if let Some(zero) = text.iter().position(|&byte| byte == 0) {
        text.truncate(zero);
    }
    CString::new(text).unwrap_or_default()
}

macro_rules! log_line {
    ($prefix:literal, $format:literal $(, $argument:expr)*) => {{
        struct Line;
        impl $crate::log::Format for Line {
            const TEXT: &'static std::ffi::CStr =
                match std::ffi::CStr::from_bytes_with_nul(concat!($prefix, $format, "\0").as_bytes()) {
                    Ok(text) => text,
                    Err(_) => panic!("a log line holds a zero byte"),
                };
        }
        $crate::log::line::<Line, _>(($($argument,)*));
    }};
}

macro_rules! log_info {
    ($format:literal $(, $argument:expr)* $(,)?) => { $crate::log::log_line!("[i] ", $format $(, $argument)*) };
}

macro_rules! log_warn {
    ($format:literal $(, $argument:expr)* $(,)?) => { $crate::log::log_line!("[w] ", $format $(, $argument)*) };
}

macro_rules! format_text {
    ($format:literal $(, $argument:expr)* $(,)?) => {{
        struct Line;
        impl $crate::log::Format for Line {
            const TEXT: &'static std::ffi::CStr =
                match std::ffi::CStr::from_bytes_with_nul(concat!($format, "\0").as_bytes()) {
                    Ok(text) => text,
                    Err(_) => panic!("a text holds a zero byte"),
                };
        }
        $crate::log::formatted::<Line, _>(($($argument,)*))
    }};
}

// What the title asked of the kernel or the GPU, and what it printed.
#[cfg_attr(not(feature = "diagnostics"), allow(unused_macros))]
macro_rules! log_kernel {
    ($format:literal $(, $argument:expr)* $(,)?) => { $crate::log::log_line!("[k] ", $format $(, $argument)*) };
}

macro_rules! log_error {
    ($format:literal $(, $argument:expr)* $(,)?) => { $crate::log::log_line!("[E] ", $format $(, $argument)*) };
}

#[cfg_attr(not(feature = "diagnostics"), allow(unused_imports))]
pub(crate) use log_kernel;
pub(crate) use {format_text, log_error, log_info, log_line, log_warn};
