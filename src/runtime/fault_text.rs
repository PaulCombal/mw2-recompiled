// A line of text from a signal handler: it is made on the stack and goes straight to
// the descriptor stderr is, with neither the C library's lock nor its buffers, which
// the thread the handler interrupted may hold.

use std::ffi::{c_int, c_void};
use std::fmt;

unsafe extern "C" {
    fn write(file: c_int, bytes: *const c_void, size: usize) -> isize;
}

pub struct Line {
    bytes: [u8; 512],
    used: usize,
}

impl Line {
    pub fn new() -> Self {
        Self { bytes: [0; 512], used: 0 }
    }

    // What does not fit is left out.
    pub fn put(&mut self, text: &[u8]) {
        let room = (self.bytes.len() - self.used).min(text.len());
        self.bytes[self.used..self.used + room].copy_from_slice(&text[..room]);
        self.used += room;
    }

    pub fn send(&self) {
        unsafe { write(2, self.bytes.as_ptr().cast(), self.used) };
    }
}

impl fmt::Write for Line {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.put(text.as_bytes());
        Ok(())
    }
}

/// %p as glibc writes it.
pub struct Pointer(pub usize);

impl fmt::Display for Pointer {
    fn fmt(&self, to: &mut fmt::Formatter) -> fmt::Result {
        if self.0 == 0 { to.write_str("(nil)") } else { write!(to, "{:#x}", self.0) }
    }
}

macro_rules! say {
    ($($text:tt)*) => {{
        let mut line = $crate::runtime::fault_text::Line::new();
        let _ = std::fmt::Write::write_fmt(&mut line, format_args!($($text)*));
        line.send();
    }};
}
pub(crate) use say;
