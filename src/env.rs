// The MW2_* switches, read as runtime/env.h does: through the C library, because
// the values the runtime's .env file gave are in its environment, and with its
// parsers, since "8x", " 8" and "1e2" are not what Rust's would make of them.

use std::ffi::{CStr, c_char, c_int};
use std::ptr;

unsafe extern "C" {
    fn getenv(name: *const c_char) -> *const c_char;
    fn strtoull(text: *const c_char, end: *mut *mut c_char, base: c_int) -> u64;
    fn strtod(text: *const c_char, end: *mut *mut c_char) -> f64;
    fn sscanf(text: *const c_char, format: *const c_char, ...) -> c_int;
    #[cfg(not(windows))]
    fn setenv(name: *const c_char, value: *const c_char, overwrite: c_int) -> c_int;
    #[cfg(windows)]
    fn _putenv_s(name: *const c_char, value: *const c_char) -> c_int;
}

/// None when unset or empty.
pub fn text(name: &CStr) -> Option<&'static CStr> {
    let value = unsafe { getenv(name.as_ptr()) };
    if value.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(value) }).filter(|value| !value.is_empty())
}

/// On when set to anything but "" or "0".
pub fn flag(name: &CStr) -> bool {
    text(name).is_some_and(|value| value != c"0")
}

pub fn number(name: &CStr, fallback: u64) -> u64 {
    text(name).map_or(fallback, |value| unsafe { strtoull(value.as_ptr(), ptr::null_mut(), 10) })
}

pub fn real(name: &CStr, fallback: f64) -> f64 {
    text(name).map_or(fallback, |value| unsafe { strtod(value.as_ptr(), ptr::null_mut()) })
}

/// The C library's parser: "1e2", "0x10" and " 8" count, and what follows is left.
pub fn parse_real(text: &CStr) -> f64 {
    unsafe { strtod(text.as_ptr(), ptr::null_mut()) }
}

/// strtoull with base 0: decimal, 0x hexadecimal or 0 octal.
pub fn parse_any_base(text: &CStr) -> u64 {
    unsafe { strtoull(text.as_ptr(), ptr::null_mut(), 0) }
}

/// Unset or empty are not the same here, unlike in text(): a set variable decides.
pub fn is_set(name: &CStr) -> bool {
    !unsafe { getenv(name.as_ptr()) }.is_null()
}

/// strtod: a number, and what is left after it.
pub fn parse_real_from(text: &CStr) -> (f64, &CStr) {
    let mut end = text.as_ptr().cast_mut();
    let value = unsafe { strtod(text.as_ptr(), &mut end) };
    (value, unsafe { CStr::from_ptr(end) })
}

/// strtoul: a number in `base` (0 reads a 0x), and what is left after it.
pub fn parse_number(text: &CStr, base: c_int) -> (u64, &CStr) {
    let mut end = text.as_ptr().cast_mut();
    let value = unsafe { strtoull(text.as_ptr(), &mut end, base) };
    (value, unsafe { CStr::from_ptr(end) })
}

/// std::env::set_var would leave the C library's copy behind on Windows, which is
/// the one getenv reads. Only while one thread runs, as what `text` gave out for this
/// name is gone afterwards.
pub fn set(name: &CStr, value: &CStr) {
    #[cfg(not(windows))]
    unsafe {
        setenv(name.as_ptr(), value.as_ptr(), 0);
    }
    #[cfg(windows)]
    unsafe {
        _putenv_s(name.as_ptr(), value.as_ptr());
    }
}

/// sscanf's "%2x": up to two hexadecimal digits, after any white space.
pub fn scan_hex_pair(text: &CStr) -> Option<u32> {
    let mut value: u32 = 0;
    (unsafe { sscanf(text.as_ptr(), c"%2x".as_ptr(), &mut value) } == 1).then_some(value)
}

/// sscanf's "+connect_lobby %llu".
pub fn scan_connect_lobby(text: &CStr) -> Option<u64> {
    let mut value: u64 = 0;
    (unsafe { sscanf(text.as_ptr(), c"+connect_lobby %llu".as_ptr(), &mut value) } == 1).then_some(value)
}

/// sscanf's "/%u/%u/%u/%u/%n": the four numbers and how far it read, when it read all
/// four and the slash after them.
pub fn scan_four_counts(text: &CStr) -> Option<([u32; 4], usize)> {
    let (mut counts, mut used) = ([0u32; 4], 0 as c_int);
    let [a, b, c, d] = &mut counts;
    let scanned = unsafe { sscanf(text.as_ptr(), c"/%u/%u/%u/%u/%n".as_ptr(), a, b, c, d, &mut used) };
    (scanned == 4 && used != 0).then_some((counts, used as usize))
}

/// sscanf's "%x=%x,%n": the pair and how far it read, when it read both and the comma.
pub fn scan_hex_assignment(text: &CStr) -> Option<(u32, u32, usize)> {
    let (mut id, mut value, mut used) = (0u32, 0u32, 0 as c_int);
    let scanned = unsafe { sscanf(text.as_ptr(), c"%x=%x,%n".as_ptr(), &mut id, &mut value, &mut used) };
    (scanned == 2 && used != 0).then_some((id, value, used as usize))
}
