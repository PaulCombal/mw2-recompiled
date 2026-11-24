// Com_Error, for a hook that gives up the way the title does.
//
// The title's error levels do not all leave. 4, 6 and 7 have paths back to the
// caller. Levels 0 to 3 have none: Com_Error (sub_82281FA0) ends in the longjmp to
// the thread's frame loop, or in Sys_Error (sub_8233D008), which restarts the title
// with XamLoaderLaunchTitle, and the console's code has no return after either
// call: the next instruction is another function's. So Level names only those,
// and a hook cannot ask for one that comes back.
//
// A longjmp skips every frame from here to the frame loop, and this hook's with
// them. That is defined here, as they hold nothing to drop: see hooks/mod.rs.

#[cfg(T_STR_CorruptedFile)]
use crate::guest::Register;
use crate::guest::{Memory, PPCContext};
use crate::log::log_error;
use crate::title::T_Com_Error;
#[cfg(T_STR_CorruptedFile)]
use crate::title::T_STR_CorruptedFile;

#[derive(Copy, Clone)]
pub enum Level {
    #[cfg_attr(not(T_GlassState_Parse), allow(dead_code))]
    Fatal = 0,
    #[cfg_attr(not(T_G_LoadMigratedGame), allow(dead_code))]
    Drop = 1,
    #[cfg_attr(not(T_CL_DisconnectReason), allow(dead_code))]
    Disconnect = 2,
}

/// Raises `format`, with the caller's r5 as its argument if it takes one. The
/// caller has set up the frame: Com_Error spills its arguments into it.
pub fn com_error(ctx: &mut PPCContext, memory: Memory, level: Level, format: u32) -> ! {
    ctx.r3.u64 = level as u64;
    ctx.r4.u64 = u64::from(format);
    T_Com_Error.call(ctx, memory);
    // The console has no instruction here. Whatever brought us back, the caller has
    // just refused what follows, so it must not run.
    log_error!("Com_Error(%u) came back, which it cannot", level as u32);
    std::process::abort()
}

/// What the title's reader of a save file raises for data it cannot read.
#[cfg(T_STR_CorruptedFile)]
pub fn corrupted(ctx: &mut PPCContext, memory: Memory) -> ! {
    ctx.r1.u64 = u64::from(ctx.r1.as_u32().wrapping_sub(0x80));
    com_error(ctx, memory, Level::Drop, T_STR_CorruptedFile) // as the reader raises it
}
