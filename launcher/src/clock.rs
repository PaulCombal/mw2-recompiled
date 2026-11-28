// The time on this machine's clock, as the C library's localtime gives it (glibc's
// localtime_r, the UCRT's _localtime64_s), written as strftime writes it in the "C" locale,
// which the launcher never leaves.
#![allow(unsafe_code)]

use std::ffi::c_int;
use std::time::{SystemTime, UNIX_EPOCH};

/// A moment as the clock shows it.
pub struct Local {
    pub year: i32,
    pub month: u32, // 1 to 12
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

// struct tm's first nine fields, which both C libraries have first; glibc's has two more.
#[repr(C)]
#[derive(Default)]
struct Tm {
    second: c_int,
    minute: c_int,
    hour: c_int,
    day: c_int,
    month: c_int,
    year: c_int,
    weekday: c_int,
    yearday: c_int,
    summer: c_int,
    #[cfg(not(windows))]
    offset: std::ffi::c_long,
    #[cfg(not(windows))]
    zone: usize,
}

#[cfg(not(windows))]
unsafe extern "C" {
    fn localtime_r(time: *const i64, tm: *mut Tm) -> *mut Tm;
}

#[cfg(windows)]
unsafe extern "C" {
    fn _localtime64_s(tm: *mut Tm, time: *const i64) -> c_int;
}

/// Seconds since 1970 on the system's clock, as time() gives them.
pub fn now() -> i64 {
    seconds(SystemTime::now())
}

/// A moment's whole seconds since 1970, rounded toward zero.
pub fn seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => after.as_secs() as i64,
        Err(before) => -(before.duration().as_secs() as i64),
    }
}

/// The moment on this machine's clock; None where the C library has none for it.
pub fn local(time: i64) -> Option<Local> {
    let mut tm = Tm::default();
    // SAFETY: a time and a struct tm to fill.
    #[cfg(not(windows))]
    let ok = !unsafe { localtime_r(&time, &mut tm) }.is_null();
    #[cfg(windows)]
    let ok = unsafe { _localtime64_s(&mut tm, &time) } == 0;
    ok.then(|| Local {
        year: tm.year + 1900,
        month: tm.month as u32 + 1,
        day: tm.day as u32,
        hour: tm.hour as u32,
        minute: tm.minute as u32,
        second: tm.second as u32,
    })
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// strftime's "%d %b %Y": "05 Mar 2026".
pub fn day(local: &Local) -> String {
    format!("{:02} {} {}", local.day, MONTHS[(local.month as usize - 1) % 12], local.year)
}

/// strftime's "%Y%m%d-%H%M%S": "20260305-181502".
pub fn stamp(local: &Local) -> String {
    format!("{}{:02}{:02}-{:02}{:02}{:02}", local.year, local.month, local.day, local.hour, local.minute, local.second)
}
