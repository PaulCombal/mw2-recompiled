// A remote peer sets the party session's slot count, which bounds a party walk.
//
// A party session stores its public and private slot counts at session+556 and
// session+552 (the order Session_Create and Session_Modify print them in). Their
// sum, read by sub_82390FF0 and directly by the session refresh (sub_8233FDA0),
// is the bound of the member-index fallback walk sub_8219C6D8, which every party
// handler uses to turn a sender into a member index. That walk indexes the
// party's 18 member records (party+64, 192 bytes each) and the session's 18-slot
// array (session+568, 48 bytes each) by any index below the sum.
//
// Both counts reach the two writers from the network unchecked: a game server's
// serverinfo sv_maxclients (any int), a system-link/lobby `connect` line's
// argv[4] (any int), a party host's partystate (two bytes, up to 510). With an
// int-sized total the walk's address math wraps and touches every 64th byte of
// the 4 GB guest space -- a multi-second stall on Linux, and on Windows, where
// guest memory commits on touch, the whole reservation is committed: an
// OOM-kill. A total above 18 is also what lets the walk return a member index
// of 18 or more, which ten PartyHost handlers then write through.
//
// The title's own contract is 18: the member scan sub_8219C818 stops at 18, the
// slot array is cleared as 18*48 bytes, canthear/nominee/migrateto reject a
// member index >= 18, and party_maxplayers / sv_maxclients / sv_privateClients
// are registered with a maximum of 18. So the counts are clamped where they are
// written, at the entry of the only two functions that store them, so no reader
// and no path that re-commits the stored total (migration keeps it; the refresh
// re-commits it) can see more than 18. A total within 18 -- every one the local
// dvars produce -- is left untouched. XAM receives the clamped counts too, so
// the session it holds agrees with the one the title stores.
//
// The counts are XAM DWORDs, so they are clamped as unsigned 32-bit values: a
// "negative" count is a huge one, and is cut to the bound as well.
//
// Multiplayer only: the campaign creates its two-player session with constant
// counts (sub_8213A430 and sub_8213A4C0 pass 0+2 and 2+0) and never modifies it.

use crate::guest::{PPCRegister, Register, guest_hook};
use crate::log::log_warn;
use crate::title::{T_Session_Create, T_Session_Modify};
use std::ffi::CStr;
use std::sync::Once;

const MAX_SLOTS: u32 = 18; // the party's member records and session slots

static LOGGED: Once = Once::new();

// Public first, then private within what is left: the sum never exceeds 18.
fn clamp_slots(public_slots: &mut PPCRegister, private_slots: &mut PPCRegister, what: &CStr) {
    let (public, private) = (public_slots.as_u32(), private_slots.as_u32());
    let clamped_public = public.min(MAX_SLOTS);
    let clamped_private = private.min(MAX_SLOTS - clamped_public);
    if clamped_public == public && clamped_private == private {
        return;
    }
    public_slots.u64 = u64::from(clamped_public);
    private_slots.u64 = u64::from(clamped_private);
    // once: whoever sends one can send thousands
    LOGGED.call_once(|| {
        log_warn!(
            "online: %s asked for %u public + %u private slots; clamped to %u + %u (max %u)",
            what,
            public,
            private,
            clamped_public,
            clamped_private,
            MAX_SLOTS
        );
    });
}

// Session_Create(session, controller, flags, sessionInfo, nonce, public, private):
// public in r8 and private in r9, kept in r25/r24 and stored at 556/552.
guest_hook!(T_Session_Create, |ctx, memory| {
    clamp_slots(&mut ctx.r8, &mut ctx.r9, c"a party session create");
    T_Session_Create.original(ctx, memory);
});

// Session_Modify(?, session, flags, public, private): public in r6 and private
// in r7, kept in r25/r29 and stored at 556/552.
guest_hook!(T_Session_Modify, |ctx, memory| {
    clamp_slots(&mut ctx.r6, &mut ctx.r7, c"a party session modify");
    T_Session_Modify.original(ctx, memory);
});
