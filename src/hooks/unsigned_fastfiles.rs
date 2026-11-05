// Unsigned fastfiles, in the multiplayer.
//
// A fastfile's first eight bytes say whether it is signed (IWff0100, as every
// file on the disc and in the title update is) or not (IWffu100, as a file
// built by anyone else is: a converted PC map). The loader starts the file's
// stream through T_DB_AuthLoadInit, which takes that as an argument and has
// both paths: with a signature it is checked, without the stream is inflated
// as it is.
//
// The multiplayer never reaches the second path. T_DB_StartStream, which
// calls it, is handed whether the file is signed, and for an unsigned one goes
// straight to its error: "Dirty disk", and the console's unreadable-disc
// screen.
//
// With MW2_CUSTOM_MAPS (the launcher's CUSTOM MAPS), the hook makes the call
// the gate skips, for an unsigned file only. A signed file takes the title's
// own path, signature check included. On a console, codxe removes the same
// branch (821B0978). Without it the gate stands, as on the console.
//
// What this lets in is a file somebody put in the game's folder under a name
// the game asks for. The title has no path that fetches a fastfile from
// another player. But nothing checks what is in it: the loader takes a zone's
// sizes, offsets and scripts as they come, so a file made to harm can do
// whatever a bug in the loader lets it -- the launcher's warning says so
// before the switch is turned on.

use crate::env;
use crate::guest::{Register, guest_hook};
use crate::log::{log_info, log_warn};
use crate::title::{T_DATA_DB_Load, T_DB_AuthLoadInit, T_DB_StartStream};
use std::sync::LazyLock;

static CUSTOM_MAPS: LazyLock<bool> = LazyLock::new(|| env::flag(c"MW2_CUSTOM_MAPS"));

guest_hook!(T_DB_StartStream, |ctx, memory| {
    let custom_maps = *CUSTOM_MAPS;
    let signed = ctx.r3.as_u32() != 0;
    let name = memory.u32_at(T_DATA_DB_Load).wrapping_add(8);
    if !signed && !custom_maps {
        log_warn!("fastfiles: %s is unsigned, and custom maps are off (MW2_CUSTOM_MAPS)", memory.text(name));
    }
    if signed || !custom_maps {
        T_DB_StartStream.original(ctx, memory);
        return;
    }
    ctx.r3.u64 = u64::from(T_DATA_DB_Load + 52);
    ctx.r4.u64 = 0;
    ctx.r5.u64 = u64::from(name);
    T_DB_AuthLoadInit.original(ctx, memory);
    if ctx.r3.as_u32() == 0 {
        log_info!("fastfiles: %s is unsigned", memory.text(name));
        return;
    }
    // The stream would not start: the title's own error, as for a signed file.
    ctx.r3.u64 = 0;
    T_DB_StartStream.original(ctx, memory);
});
