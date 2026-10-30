//! A line for each thing that happens, with the time in UTC: to the standard
//! output, and to --log-file for a Docker that shows no container output.

use std::fs::File;
use std::io::{self, Write};
use std::sync::Mutex;
use time::OffsetDateTime;

static FILE: Mutex<Option<File>> = Mutex::new(None);

pub fn to_file(file: File) {
    if let Ok(mut held) = FILE.lock() {
        *held = Some(file);
    }
}

pub fn line(text: &str) {
    let now = OffsetDateTime::now_utc();
    let line = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {text}\n",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    );
    // A log that cannot be written must not stop the players.
    let _ = io::stdout().write_all(line.as_bytes());
    let _ = io::stdout().flush();
    if let Ok(mut held) = FILE.lock()
        && let Some(file) = held.as_mut()
    {
        let _ = file.write_all(line.as_bytes());
    }
}

macro_rules! log {
    ($($argument:tt)*) => { $crate::log::line(&format!($($argument)*)) };
}
pub(crate) use log;
