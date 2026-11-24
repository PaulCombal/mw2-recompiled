// MW2_FPS_LIMIT: how many frames a second the title draws at most.
//
// Unset or 60 is the console's pacing: D3D retires a frame at the display's
// blank and the title's own limiter (com_maxfps) is at its 60. Any other
// number takes the title off the blank the way the console's D3D offers it
// (hooks/d3d_hooks.rs), with that number as com_maxfps; 0 is no limit. The title's
// limiter counts whole milliseconds a frame, so the rate it keeps is 1000
// over a whole number: 120 gives 125, 144 gives 166.

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
