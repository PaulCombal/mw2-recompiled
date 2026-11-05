// A line of a route or a flight, read by sscanf, so that a number reads as the C++ read
// it, and the lines of a file cut where fgets cut them.

use crate::log::c_string;
use std::ffi::{CStr, CString, c_char, c_int};

unsafe extern "C" {
    fn sscanf(text: *const c_char, format: *const c_char, ...) -> c_int;
}

/// `<word> x y z yaw pitch`. Fields read before a failure stay read.
pub fn word_and_reals(line: &CStr, word: &mut [u8; 8], values: &mut [f32; 5]) -> i32 {
    let [x, y, z, yaw, pitch] = values;
    unsafe {
        sscanf(
            line.as_ptr(),
            c"%7s %f %f %f %f %f".as_ptr(),
            word.as_mut_ptr(),
            x as *mut f32,
            y as *mut f32,
            z as *mut f32,
            yaw as *mut f32,
            pitch as *mut f32,
        )
    }
}

/// `x y z yaw pitch`, of which as many as it says.
pub fn reals(line: &CStr, values: &mut [f32; 5]) -> i32 {
    let [x, y, z, yaw, pitch] = values;
    unsafe {
        sscanf(
            line.as_ptr(),
            c"%f %f %f %f %f".as_ptr(),
            x as *mut f32,
            y as *mut f32,
            z as *mut f32,
            yaw as *mut f32,
            pitch as *mut f32,
        )
    }
}

/// `seconds x y z yaw pitch`, whole or not at all.
pub fn moment(line: &CStr, time: &mut f64, values: &mut [f32; 5]) -> bool {
    let [x, y, z, yaw, pitch] = values;
    let read = unsafe {
        sscanf(
            line.as_ptr(),
            c"%lf %f %f %f %f %f".as_ptr(),
            time as *mut f64,
            x as *mut f32,
            y as *mut f32,
            z as *mut f32,
            yaw as *mut f32,
            pitch as *mut f32,
        )
    };
    read == 6
}

/// What fgets into a buffer of `room` bytes reads, line by line.
pub fn lines(file: &[u8], room: usize) -> impl Iterator<Item = CString> {
    let mut left = file;
    std::iter::from_fn(move || {
        if left.is_empty() {
            return None;
        }
        let most = left.len().min(room - 1);
        let size = left[..most].iter().position(|&byte| byte == b'\n').map_or(most, |newline| newline + 1);
        let (line, rest) = left.split_at(size);
        left = rest;
        Some(c_string(line))
    })
}
