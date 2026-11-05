// MW2_FPS_LIMIT: see runtime/frame_rate.h.

use crate::env;

const CONSOLE: u64 = 60;

/// com_maxfps: 0 is no limit, and 1000 is the most the title takes.
pub fn limit() -> u32 {
    env::number(c"MW2_FPS_LIMIT", CONSOLE).min(1000) as u32
}

/// Frames are retired at the blank, one a blank, as on the console.
#[cfg(feature = "diagnostics")]
pub fn console() -> bool {
    u64::from(limit()) == CONSOLE
}
