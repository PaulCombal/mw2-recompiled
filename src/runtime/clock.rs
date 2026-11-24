// The time the diagnostics count in: nanoseconds on the steady clock. Never 0, which
// what keeps a time uses for not set.

use std::sync::OnceLock;
use std::time::Instant;

// Far from 0, as a time so close to it would read as just after an unset one.
pub const ORIGIN: u64 = 1 << 50;

static BEGAN: OnceLock<Instant> = OnceLock::new();

/// First thing in main, so that the time is counted from the start.
pub fn start() {
    BEGAN.get_or_init(Instant::now);
}

pub fn nanoseconds() -> u64 {
    ORIGIN + BEGAN.get_or_init(Instant::now).elapsed().as_nanos() as u64
}
