// XAM -- the dashboard-facing surface. Without an online service that carries lobbies,
// nothing here talks to anything like Xbox Live; the aim is to answer definitively so the
// title takes its offline path instead of polling or erroring, and to host the
// multiplayer's system-link session. With one (online::live()), the player is signed in
// to Live and XAM stands in for the parts of it a private match needs: sessions,
// invitations, and Live storage kept on this machine.

use super::content::{SAVE_DEVICE_ID, save_root};
use super::misc::{ERROR_NOT_FOUND, ERROR_SUCCESS};
use super::net::{self, XNADDR_SIZE, XnAddr};
use super::objects::{self, Kind, Object};
use super::{allocate_guest, complete_overlapped, complete_overlapped_ex, free_guest, random_bytes};
use crate::guest::{Memory, Register, kernel_import};
use crate::log::{c_string, log_error, log_info, log_warn};
use crate::runtime::online::{self, SESSION_INFO_SIZE, Session as SessionInfo, SessionListing, SessionNumber};
use crate::runtime::online::{SessionFound, SessionSearch, SessionWant};
use crate::runtime::{friends, presence, signin};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

const ERROR_INVALID_HANDLE: u32 = 0x0006;
const ERROR_NOT_ENOUGH_MEMORY: u32 = 0x0008;
const ERROR_NO_MORE_FILES: u32 = 0x0012;
const ERROR_INVALID_PARAMETER: u32 = 0x0057;
const ERROR_IO_PENDING: u32 = 0x03E5;
const ERROR_CANCELLED: u32 = 1223;
const ERROR_NO_SUCH_USER: u32 = 0x0525;
const ERROR_FUNCTION_FAILED: u32 = 1627;
const ERROR_NOT_LOGGED_ON: u32 = 1245;
const STATUS_NO_MEMORY: u32 = 0xC000_0017;
const E_INVALIDARG: u32 = 0x8007_0057;

const LOCAL_USER: u32 = 0;
const TITLE_ID: u32 = 0x4156_0817;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---- who is signed in -------------------------------------------------------------

const SIGNED_IN_LOCALLY: u32 = 1;
const SIGNED_IN_TO_LIVE: u32 = 2;
const ACCOUNT_BITS: u64 = 0xFFFF_FFFF_FFFF;
const OFFLINE_XUID: u64 = 0xE000_0000_0000_0000;

// The player at the first controller: the profile there (signin), which the online
// service knows by the same number -- plus one for a second copy on this machine. An
// offline XUID (top byte 0xE0) over its 48 bits.
fn local_account() -> u64 {
    online::get().map_or_else(|| signin::first().id, |service| service.account()) & ACCOUNT_BITS
}

fn local_xuid() -> u64 {
    OFFLINE_XUID | local_account()
}

// The XUID Live knows an account by, the console's 0x0009 prefix over the same 48 bits.
fn online_xuid(account: u64) -> u64 {
    0x0009_0000_0000_0000 | (account & ACCOUNT_BITS)
}

fn local_online_xuid() -> u64 {
    online_xuid(local_account())
}

fn xuid_text(xuid: u64) -> String {
    format!("{xuid:016x}")
}

// The name the service has for the player -- Steam's -- or the profile's.
fn local_name() -> Vec<u8> {
    let mut name = online::get().map(|service| service.local_name()).unwrap_or_default();
    if name.is_empty() {
        name = signin::first().name;
    }
    name.truncate(15); // a gamertag's longest
    name
}

// Who is at a controller. The first is the player above, signed in to Live when the
// online service carries lobbies. The others are whoever the sign-in screen put there
// (signin): a profile of this machine, which is local and offline.
#[derive(Default)]
struct Who {
    /// 0 nobody, 1 local, 2 Live
    state: u32,
    offline: u64,
    online: u64,
    name: Vec<u8>,
}

fn who_is(user: u32) -> Who {
    if user == LOCAL_USER {
        let live = online::live();
        return Who {
            state: if live { SIGNED_IN_TO_LIVE } else { SIGNED_IN_LOCALLY },
            offline: local_xuid(),
            online: if live { local_online_xuid() } else { 0 },
            name: local_name(),
        };
    }
    let player = signin::at(user);
    if !player.signed_in {
        return Who::default();
    }
    Who { state: SIGNED_IN_LOCALLY, offline: OFFLINE_XUID | player.id, online: 0, name: player.name }
}

// What snprintf(out, room, "%s", text) leaves: as much as fits, and a NUL.
fn store_text(memory: Memory, address: u32, room: u32, text: &[u8]) {
    let length = text.len().min(room.max(1) as usize - 1);
    memory.store_bytes(address, &text[..length]);
    memory.store_u8(address.wrapping_add(length as u32), 0);
}

// ---- notifications ----------------------------------------------------------------

// XNotify. A listener asks for areas of notification -- system, Live, friends -- and the
// title pumps it once a frame. The area of a notification is in its id's top bits.
struct NotifyListener {
    areas: u64,
    /// id, param
    waiting: Mutex<VecDeque<(u32, u32)>>,
}

struct Listeners {
    list: Vec<Weak<Object>>,
    // An invitation accepted before the title listens for Live -- as when the console
    // starts a game to take one up -- waits for its first listener.
    invite_unheard: bool,
}

static LISTENERS: Mutex<Listeners> = Mutex::new(Listeners { list: Vec::new(), invite_unheard: false });

const XN_LIVE_CONNECTIONCHANGED: u32 = 0x0200_0001;
const XN_LIVE_INVITE_ACCEPTED: u32 = 0x0200_0002;
const X_ONLINE_S_LOGON_CONNECTION_ESTABLISHED: u32 = 0x0015_10F0;
const AREA_LIVE: u64 = 1 << 1;

fn area_of(id: u32) -> u64 {
    1 << ((id >> 25) & 0x3F)
}

fn listener_of(object: &Object) -> Option<&NotifyListener> {
    match &object.kind {
        Kind::Other(other) => other.downcast_ref::<NotifyListener>(),
        _ => None,
    }
}

// False when nobody listens for it yet.
fn post_held(listeners: &Listeners, id: u32, param: u32) -> bool {
    let mut heard = false;
    for object in listeners.list.iter().filter_map(Weak::upgrade) {
        if let Some(listener) = listener_of(&object).filter(|listener| listener.areas & area_of(id) != 0) {
            lock(&listener.waiting).push_back((id, param));
            heard = true;
        }
    }
    heard
}

/// Tells every listener for its area of a notification.
pub fn post_notification(id: u32, param: u32) {
    #[cfg(mw2_screens_rig)]
    return screens_rig(c"notify", id, param);
    #[allow(unreachable_code)]
    post_held(&lock(&LISTENERS), id, param);
}

// tools/runtime_diff.py's screens have no XAM: it writes down what they told it.
#[cfg(mw2_screens_rig)]
#[allow(unsafe_code)]
fn screens_rig(what: &std::ffi::CStr, id: u32, param: u32) {
    unsafe extern "C" {
        fn rig_xam_told(what: *const std::ffi::c_char, id: u32, param: u32);
    }
    unsafe { rig_xam_told(what.as_ptr(), id, param) }
}

// (areas, version)
kernel_import!(XamNotifyCreateListener, |ctx, _memory| {
    let areas = ctx.r3.as_u64();
    let mut waiting = VecDeque::new();
    // Signed in to Live, the connection came up before the title was there to hear it;
    // the console tells a new listener so. The title asks for its NAT type and measures
    // its line when it hears it.
    if online::live() && areas & AREA_LIVE != 0 {
        waiting.push_back((XN_LIVE_CONNECTIONCHANGED, X_ONLINE_S_LOGON_CONNECTION_ESTABLISHED));
    }
    let object = Object::new(Kind::Other(Box::new(NotifyListener { areas, waiting: Mutex::new(waiting) })));
    {
        let mut listeners = lock(&LISTENERS);
        listeners.list.retain(|weak| weak.strong_count() > 0);
        listeners.list.push(Arc::downgrade(&object));
        if listeners.invite_unheard && areas & area_of(XN_LIVE_INVITE_ACCEPTED) != 0 {
            let listener = listener_of(&object).expect("made as one");
            lock(&listener.waiting).push_back((XN_LIVE_INVITE_ACCEPTED, 0));
            listeners.invite_unheard = false;
        }
    }
    ctx.r3.u64 = u64::from(objects::lock().insert(object));
});

// (listener, onlyThisId or 0, idOut, paramOut): 1 when one was taken.
kernel_import!(XNotifyGetNext, |ctx, memory| {
    let (only, id_out, param_out) = (ctx.r4.as_u32(), ctx.r5.as_u32(), ctx.r6.as_u32());
    let object = objects::lock().lookup(ctx.r3.as_u32()).cloned();
    if id_out != 0 {
        memory.store_u32(id_out, 0);
    }
    if param_out != 0 {
        memory.store_u32(param_out, 0);
    }
    ctx.r3.u64 = 0;
    let Some(listener) = object.as_deref().and_then(listener_of) else { return };
    let mut waiting = lock(&listener.waiting);
    let Some(at) = waiting.iter().position(|&(id, _)| only == 0 || id == only) else { return };
    let (id, param) = waiting.remove(at).expect("found");
    if id == XN_LIVE_INVITE_ACCEPTED {
        log_info!("xam: the title heard of the accepted invitation");
    }
    if id_out != 0 {
        memory.store_u32(id_out, id);
    }
    if param_out != 0 {
        memory.store_u32(param_out, param);
    }
    ctx.r3.u64 = 1;
});

kernel_import!(XNotifyPositionUI, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

// ---- users ------------------------------------------------------------------------

// Zero for nobody, one for a local profile, two for a player on Live.
kernel_import!(XamUserGetSigninState, |ctx, _memory| {
    ctx.r3.u64 = u64::from(who_is(ctx.r3.as_u32()).state);
});

// (user, typeMask, xuidOut). Mask 1 asks for the offline XUID, 2 and 4 for the online
// one, which a player signed in to Live has.
kernel_import!(XamUserGetXUID, |ctx, memory| {
    let who = who_is(ctx.r3.as_u32());
    if who.state == 0 {
        ctx.r3.u64 = u64::from(ERROR_NO_SUCH_USER);
        return;
    }
    let out = ctx.r5.as_u32();
    if out != 0 {
        memory.store_u64(out, if who.online != 0 && ctx.r4.as_u32() & 6 != 0 { who.online } else { who.offline });
    }
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

kernel_import!(XamUserGetName, |ctx, memory| {
    let who = who_is(ctx.r3.as_u32());
    if who.state == 0 {
        ctx.r3.u64 = u64::from(ERROR_NO_SUCH_USER);
        return;
    }
    let name = ctx.r4.as_u32();
    if name != 0 {
        store_text(memory, name, ctx.r5.as_u32(), &who.name);
    }
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

// (user, flags, infoOut). Flags 1 asks for the online XUID only, 2 for the offline one;
// with neither, a Live player is named by the online one.
// XUSER_SIGNIN_INFO { xuid, flags, state, guestNumber, sponsorIndex, name[16] }.
kernel_import!(XamUserGetSigninInfo, |ctx, memory| {
    const OFFLINE_ONLY: u32 = 2;
    const LIVE_ENABLED: u32 = 1;
    let who = who_is(ctx.r3.as_u32());
    if who.state == 0 {
        ctx.r3.u64 = u64::from(ERROR_NO_SUCH_USER);
        return;
    }
    let info = ctx.r5.as_u32();
    if info != 0 {
        memory.zero(info, 40);
        let online = who.online != 0 && ctx.r4.as_u32() & OFFLINE_ONLY == 0;
        memory.store_u64(info, if online { who.online } else { who.offline });
        memory.store_u32(info.wrapping_add(8), if who.online != 0 { LIVE_ENABLED } else { 0 });
        memory.store_u32(info.wrapping_add(12), who.state);
        store_text(memory, info.wrapping_add(24), 16, &who.name);
    }
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

// (user, privilege, result). Signed in to Live, the player has them all. A profile that
// is not on Live is not logged on, which the title tells from a privilege it was
// refused.
kernel_import!(XamUserCheckPrivilege, |ctx, memory| {
    let user = ctx.r3.as_u32();
    let local = (0..4).any(|index| (user == index || user == 0xFF) && who_is(index).state == SIGNED_IN_LOCALLY);
    let result = ctx.r5.as_u32();
    if result != 0 {
        memory.store_u32(result, u32::from(!local && who_is(user).state == SIGNED_IN_TO_LIVE));
    }
    ctx.r3.u64 = u64::from(if local { ERROR_NOT_LOGGED_ON } else { ERROR_SUCCESS });
});

kernel_import!(XamUserAreUsersFriends, |ctx, memory| {
    let result = ctx.r5.as_u32();
    if result != 0 {
        memory.store_u32(result, 0);
    }
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

// The profile settings calls are in profile.rs.

kernel_import!(XamUserCreateStatsEnumerator, |ctx, _memory| {
    ctx.r3.u64 = u64::from(ERROR_NOT_FOUND);
});

// ---- the system's screens ---------------------------------------------------------

// (panes, flags). The console's sign-in screen, which is signin's here; the title hears
// of what was chosen as a change of who is signed in.
kernel_import!(XamShowSigninUI, |ctx, _memory| {
    signin::ask();
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

// Choosing where to save. This is the dialog behind "Unable to Write to Default Save
// Device": until it names a device the title has nowhere to put a save, so it asks again
// before every level.
//
// The title accepts exactly one answer from this call. It is asynchronous, and the caller
// tests the return for ERROR_IO_PENDING and then polls XGetOverlappedResult; anything
// else -- success included -- it reads as a refusal and goes back to the prompt. So the
// choice is made here and reported through the overlapped block, and the call itself
// says "pending".
//
// (userIndex, contentType, contentFlags, bytesNeeded, deviceIdOut, overlapped)
kernel_import!(XamShowDeviceSelectorUI, |ctx, memory| {
    // The hard disk, as the content layer reports it.
    let device = ctx.r7.as_u32();
    if device != 0 {
        memory.store_u32(device, SAVE_DEVICE_ID);
    }
    let overlapped = ctx.r8.as_u32();
    if overlapped == 0 {
        ctx.r3.u64 = u64::from(ERROR_SUCCESS);
        return;
    }
    complete_overlapped(memory, overlapped, ERROR_SUCCESS);
    ctx.r3.u64 = u64::from(ERROR_IO_PENDING);
});

kernel_import!(XamShowMessageBoxUIEx, |ctx, _memory| {
    ctx.r3.u64 = u64::from(ERROR_NOT_FOUND);
});

// The lobby's "Invite friends": the friends screen.
kernel_import!(XamShowFriendsUI, |ctx, _memory| {
    friends::ask();
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

kernel_import!(XamShowGamerCardUIForXUID, |ctx, _memory| {
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

kernel_import!(XamShowDirtyDiscErrorUI, |ctx, _memory| {
    log_error!("the title reported a dirty disc and asked for the error UI");
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

// ---- sessions ---------------------------------------------------------------------

// XAM's message ports. Everything the title asks of the multiplayer services --
// sessions, matchmaking, statistics -- arrives here as (app, message, buffer) rather than
// as its own import, so an entry point named XSessionCreate in the engine is app 0xFB
// message 0x000B0010 by the time it reaches the kernel. Answering "success" and writing
// nothing is not neutral: the title reads the session back out and gives up on what it
// finds.
const APP_XGI: u32 = 0xFB; // the game-invite / session app
const APP_XLIVEBASE: u32 = 0xFC; // Live itself: storage, invitations, NAT

// What a message answers: its result, and for an asynchronous one the extended error the
// caller reads from the overlapped block.
#[derive(Clone, Copy)]
struct Reply {
    result: u32,
    extended: u32,
    /// the overlapped block is completed later
    pending: bool,
}

const DONE: Reply = Reply { result: ERROR_SUCCESS, extended: ERROR_SUCCESS, pending: false };

fn result(result: u32) -> Reply {
    Reply { result, extended: result, pending: false }
}

// XSESSION_INFO, the block XSessionCreate fills in for the caller: XNKID, XNADDR, XNKEY.
const SESSION_ID: usize = 0;
const HOST_ADDRESS: usize = 8;
const KEY: usize = 44;
const _: () = assert!(KEY + 16 == SESSION_INFO_SIZE && HOST_ADDRESS + XNADDR_SIZE == KEY);

fn host_address(info: &SessionInfo) -> XnAddr {
    info[HOST_ADDRESS..KEY].try_into().expect("36 bytes")
}

fn session_id(info: &SessionInfo) -> [u8; 8] {
    info[SESSION_ID..HOST_ADDRESS].try_into().expect("8 bytes")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

// XSESSION_CREATE_* flags.
const HOST: u32 = 0x01;
const PRESENCE: u32 = 0x02;
const STATS: u32 = 0x04;
const MATCHMAKING: u32 = 0x08;
const INVITES_DISABLED: u32 = 0x100;
const JOIN_IN_PROGRESS_DISABLED: u32 = 0x400;
const LIVE_FEATURES: u32 = 0x02 | 0x04 | 0x08 | 0x10; // presence, stats, matchmaking, arbitration

// Every session the title has made, by the session object it names them with: what
// another machine needs to join one, and the order they came in.
#[derive(Clone)]
struct Session {
    flags: u32,
    info: SessionInfo,
    public_slots: u32,
    private_slots: u32,
    nonce: [u8; 8],
    made: u64,
    /// a system-link game this machine hosts
    system_link: bool,
    /// and it is on: from its creation or start to its end
    on: bool,
    /// who joined, and whether in a private slot
    members: BTreeMap<u64, bool>,
}

// The contexts and properties the title's searches compare, as the player last set them:
// the game mode, ranked or not and the playlist; the protocol, the map packs, the
// playlists' version and the room for a party. What Live kept with a session comes from
// these on the console too.
const PARTY_ROOM: u32 = 0x1000_001A;
const SEARCHED: [u32; 7] = [0x800B, 0x800A, 0x0006, 0x1000_0014, 0x1000_0015, 0x1000_0017, PARTY_ROOM];

struct Sessions {
    by_object: BTreeMap<u32, Session>,
    made: u64,
    searched: BTreeMap<u32, u32>,
    /// the id of the session last handed to the service
    joinable: [u8; 8],
}

static SESSIONS: Mutex<Sessions> =
    Mutex::new(Sessions { by_object: BTreeMap::new(), made: 0, searched: BTreeMap::new(), joinable: [0; 8] });
// Held from choosing what to list to telling the service.
static LISTING: Mutex<()> = Mutex::new(());
// Held from choosing what to advertise to telling the service: the connect line last
// told it.
static ADVERT: Mutex<String> = Mutex::new(String::new());

// A new identity for a session this machine hosts, the way the console makes one. The
// top bits of the id say what kind of session it is: clear for system link, 0x80 for one
// peers hold over Live. A session id of all zeroes is what the title reads as "the
// service never gave us one". A player sent to a friend's private match gets the flags of
// a Find Game session, as they are the host's to know. So the host says which it is in
// the id, which every player is given however they came and which stays through a
// migration.
const FIND_GAME_SESSION: u32 = STATS | MATCHMAKING;
const FIND_GAME_ID: u8 = 0x01; // in the id's last byte

fn is_find_game(session: &Session) -> bool {
    session.flags & FIND_GAME_SESSION == FIND_GAME_SESSION && session.info[SESSION_ID + 7] & FIND_GAME_ID != 0
}

fn make_host_info(live: bool, find_game: bool) -> SessionInfo {
    let mut info = [0; SESSION_INFO_SIZE];
    random_bytes(&mut info[SESSION_ID..HOST_ADDRESS]);
    let id = &mut info[SESSION_ID..HOST_ADDRESS];
    id[0] = if live { 0x80 | (id[0] & 0x0F) } else { (id[0] & 0x7F) | 0x01 };
    id[7] = (id[7] & !FIND_GAME_ID) | if find_game { FIND_GAME_ID } else { 0 };
    info[HOST_ADDRESS..KEY].copy_from_slice(&net::title_xn_addr());
    random_bytes(&mut info[KEY..]);
    info
}

// What another machine types to join a system-link game: the console command the title's
// own server browser issues, with what the host's reply to a search carries -- XNADDR,
// key and id in hex, the slots and the nonce.
fn connect_line(session: &Session) -> String {
    format!(
        "connect {} {} {} {} {} {}",
        hex(&session.info[HOST_ADDRESS..KEY]),
        hex(&session.info[KEY..]),
        hex(&session.info[SESSION_ID..HOST_ADDRESS]),
        session.public_slots,
        session.private_slots,
        hex(&session.nonce)
    )
}

// The newest session (by when it was made) of those `wanted` picks.
fn newest(sessions: &Sessions, wanted: impl Fn(&Session) -> bool) -> Option<&Session> {
    sessions.by_object.values().filter(|session| wanted(session)).max_by_key(|session| session.made)
}

// The system-link game this machine hosts: the newest such session that is on, from its
// creation (the server spawning a map) or start to its end (the server shutting down, or
// the map ending). Told to the service whenever the sessions change, under one lock from
// the choice to the telling, so that two threads cannot tell it in the other order.
fn publish_hosted() {
    let Some(service) = online::get() else { return };
    let mut advert = lock(&ADVERT);
    let line =
        newest(&lock(&SESSIONS), |session| session.system_link && session.on).map(connect_line).unwrap_or_default();
    if line == *advert {
        return;
    }
    *advert = line;
    if advert.is_empty() {
        log_info!("online: no longer hosting");
    } else {
        log_info!("online: hosting; another machine joins with \"%s\"", c_string(advert.as_bytes()).as_c_str());
    }
    service.advertise(advert.as_bytes());
}

// What matchmaking lists: the newest session this machine hosts that uses matchmaking and
// stats, which the party's own session (0x29) does not. A session that takes nobody once
// it has started leaves the list then, as it leaves Live's. A lobby is listed from its
// creation, with no public slots until the title's searches have failed and it opens
// them.
fn publish_listing() {
    let Some(service) = online::get().filter(|service| service.matchmaking()) else { return };
    let _listing = lock(&LISTING);
    let (listing, playing) = {
        let sessions = lock(&SESSIONS);
        const LISTED: u32 = HOST | STATS | MATCHMAKING;
        let playing = sessions.by_object.values().any(is_find_game);
        let best = newest(&sessions, |session| {
            is_find_game(session)
                && session.flags & LISTED == LISTED
                && !(session.on && session.flags & JOIN_IN_PROGRESS_DISABLED != 0)
        });
        let listing = best.map(|best| {
            let filled_private = best.members.values().filter(|&&in_private| in_private).count() as u32;
            let filled_public = best.members.len() as u32 - filled_private;
            SessionListing {
                session: best.info,
                open_public: best.public_slots - best.public_slots.min(filled_public),
                open_private: best.private_slots - best.private_slots.min(filled_private),
                filled_public,
                filled_private,
                numbers: sessions.searched.iter().map(|(&id, &value)| SessionNumber { id, value }).collect(),
            }
        });
        (listing, playing)
    };
    service.list(listing.as_ref());
    service.in_game(playing);
}

// What a friend joins over Live: the newest session this machine hosts that shows in its
// presence and takes invitations -- the party's while the player is in the Xbox LIVE
// menu, the private match's once in its lobby. Told to the service whenever the sessions
// change. The console line is the title's own way in to the same place, logged for
// testing.
fn publish_joinable() {
    if !online::live() {
        return;
    }
    let joinable = {
        let mut sessions = lock(&SESSIONS);
        let best = newest(&sessions, |session| {
            session.flags & HOST != 0 && session.flags & PRESENCE != 0 && session.flags & INVITES_DISABLED == 0
        })
        .map(|best| best.info);
        let id = best.as_ref().map_or([0; 8], session_id);
        if id == sessions.joinable {
            return;
        }
        sessions.joinable = id;
        best
    };
    match &joinable {
        Some(info) => log_info!(
            "online: joinable; another machine joins with \"xpartyjoin %s %s %s\"",
            c_string(hex(&info[HOST_ADDRESS..KEY]).as_bytes()).as_c_str(),
            c_string(hex(&info[KEY..]).as_bytes()).as_c_str(),
            c_string(hex(&info[SESSION_ID..HOST_ADDRESS]).as_bytes()).as_c_str()
        ),
        None => log_info!("online: nothing to join"),
    }
    if let Some(service) = online::get() {
        service.set_joinable(joinable.as_ref());
    }
}

fn words(memory: Memory, buffer: u32, count: u32) -> Vec<u32> {
    (0..count).map(|i| memory.u32_at(buffer.wrapping_add(4 * i))).collect()
}

fn session_at(memory: Memory, address: u32) -> SessionInfo {
    let mut info = [0; SESSION_INFO_SIZE];
    memory.load_bytes(address, &mut info);
    info
}

// (session, flags, publicSlots, privateSlots, xuid, sessionInfoOut, nonceOut)
fn xgi_session_create(memory: Memory, buffer: u32, length: u32) -> Reply {
    if length < 28 || buffer == 0 {
        return result(ERROR_INVALID_PARAMETER);
    }
    let words = words(memory, buffer, 7);
    let (object, flags) = (words[0], words[1]);

    // A session this console hosts on its own -- system link -- gets an identity the
    // title invents no differently than the console would. One that wants Xbox Live
    // needs a service that carries lobbies; without one it is refused, and the title
    // falls back to the offline path instead of waiting on a service that never answers.
    let live = flags & LIVE_FEATURES != 0;
    if live && !online::live() {
        return result(0x8015_5209); // X_ONLINE_E_SESSION_NOT_LOGGED_ON
    }

    let (info, nonce) = (words[5], words[6]);
    // Joining someone else's session: the title passes the host's session info and nonce
    // in, from the game it found, and reads them back.
    let host = flags == 0 || flags & HOST != 0;
    if host {
        if info != 0 {
            memory.store_bytes(info, &make_host_info(live, flags & FIND_GAME_SESSION == FIND_GAME_SESSION));
        }
        if nonce != 0 {
            let mut bytes = [0; 8];
            random_bytes(&mut bytes);
            memory.store_bytes(nonce, &bytes);
        }
    }
    let mut session = Session {
        flags,
        info: if info != 0 { session_at(memory, info) } else { [0; SESSION_INFO_SIZE] },
        public_slots: words[2],
        private_slots: words[3],
        nonce: [0; 8],
        made: 0,
        system_link: host && info != 0 && nonce != 0 && !live,
        on: false,
        members: BTreeMap::new(),
    };
    session.on = session.system_link;
    if nonce != 0 {
        memory.load_bytes(nonce, &mut session.nonce);
    }
    let (public_slots, private_slots, id) = (session.public_slots, session.private_slots, session_id(&session.info));
    {
        let mut sessions = lock(&SESSIONS);
        sessions.made += 1;
        session.made = sessions.made;
        sessions.by_object.insert(object, session);
    }
    if info != 0 {
        net::note_session_id(id);
    }
    log_info!(
        "online: session %08x created, flags %x, %u public and %u private slots",
        object,
        flags,
        public_slots,
        private_slots
    );
    publish_hosted();
    publish_joinable();
    publish_listing();
    DONE
}

// (session, info, user, ...): the session has another host. With a player's index that
// is this machine, which fills in `info` for the title to send the others; with
// XUSER_INDEX_NONE `info` is the new host's. Only the host's address changes: everyone
// keeps the session's id and key.
fn xgi_session_migrate_host(memory: Memory, buffer: u32, length: u32) -> Reply {
    if buffer == 0 || length < 12 {
        return result(ERROR_INVALID_PARAMETER);
    }
    let words = words(memory, buffer, 3);
    let info = words[1];
    if info == 0 {
        return result(ERROR_INVALID_PARAMETER);
    }
    let host = words[2] < 4;
    {
        let mut sessions = lock(&SESSIONS);
        let Some(session) = sessions.by_object.get_mut(&words[0]) else { return result(ERROR_INVALID_PARAMETER) };
        if host {
            session.info[HOST_ADDRESS..KEY].copy_from_slice(&net::title_xn_addr());
            memory.store_bytes(info, &session.info);
            session.flags |= HOST;
        } else {
            session.info = session_at(memory, info);
            session.flags &= !HOST;
        }
    }
    log_info!(
        "online: session %08x migrated, this machine %s",
        words[0],
        if host { c"hosts it now" } else { c"follows the new host" }
    );
    publish_joinable();
    publish_listing();
    if !host
        && let Some(peer) = net::peer_of(&host_address(&session_at(memory, info)))
        && let Some(service) = online::get()
    {
        service.host_moved(peer);
    }
    DONE
}

fn set_on(object: u32, on: bool) {
    {
        let mut sessions = lock(&SESSIONS);
        let Some(session) = sessions.by_object.get_mut(&object) else { return };
        session.on = on;
    }
    publish_hosted();
    publish_listing();
}

// (session, flags, ...)
fn xgi_session_start(memory: Memory, buffer: u32) -> Reply {
    if buffer != 0 {
        set_on(memory.u32_at(buffer), true);
    }
    DONE
}

// (session, 0)
fn xgi_session_end(memory: Memory, buffer: u32) -> Reply {
    if buffer != 0 {
        set_on(memory.u32_at(buffer), false);
    }
    DONE
}

// (session, ...): the session goes, and with it what a friend could join.
fn xgi_session_delete(memory: Memory, buffer: u32) -> Reply {
    if buffer != 0 {
        lock(&SESSIONS).by_object.remove(&memory.u32_at(buffer));
    }
    publish_hosted();
    publish_joinable();
    publish_listing();
    DONE
}

// (session, count, xuids, 0, private flags): players joining this session on this
// machine's say-so. It counts them, which is all Live did for the slots a search sees.
fn xgi_session_join_remote(memory: Memory, buffer: u32, length: u32) -> Reply {
    if buffer == 0 || length < 20 {
        return result(ERROR_INVALID_PARAMETER);
    }
    let words = words(memory, buffer, 5);
    let (xuids, in_private) = (words[2], words[4]);
    {
        let mut sessions = lock(&SESSIONS);
        if let Some(session) = sessions.by_object.get_mut(&words[0]).filter(|_| xuids != 0) {
            for i in 0..words[1] {
                let private = in_private != 0 && memory.u32_at(in_private.wrapping_add(i.wrapping_mul(4))) != 0;
                session.members.insert(memory.u64_at(xuids.wrapping_add(i.wrapping_mul(8))), private);
            }
        }
    }
    publish_listing();
    DONE
}

// (session, count, xuids, 0, 0)
fn xgi_session_leave_remote(memory: Memory, buffer: u32, length: u32) -> Reply {
    if buffer == 0 || length < 12 {
        return result(ERROR_INVALID_PARAMETER);
    }
    let words = words(memory, buffer, 3);
    let xuids = words[2];
    {
        let mut sessions = lock(&SESSIONS);
        if let Some(session) = sessions.by_object.get_mut(&words[0]).filter(|_| xuids != 0) {
            for i in 0..words[1] {
                session.members.remove(&memory.u64_at(xuids.wrapping_add(i.wrapping_mul(8))));
            }
        }
    }
    publish_listing();
    DONE
}

fn note_searched(id: u32, value: u32) {
    if !SEARCHED.contains(&id) {
        return;
    }
    lock(&SESSIONS).searched.insert(id, value);
    publish_listing();
}

// (user, pad, xuid, property, size, value): the title sets one the way it sets a context,
// and only numbers of 32 bits are compared.
fn xgi_user_set_property(memory: Memory, buffer: u32) -> Reply {
    if buffer == 0 {
        return result(ERROR_INVALID_PARAMETER);
    }
    let words = words(memory, buffer, 7);
    if words[5] == 4 && words[6] != 0 {
        note_searched(words[4], memory.u32_at(words[6]));
    }
    DONE
}

// Searches the title is waiting on, and what it asked for.
struct Search {
    id: u32,
    overlapped: u32,
    results: u32,
    capacity: u32,
    most: u32,
}

static SEARCHES: Mutex<Vec<Search>> = Mutex::new(Vec::new());

const FINDING_SIZE: u32 = 92; // XSESSION_SEARCHRESULT

// (procedure, user, most, properties << 16 | contexts, properties, contexts, size of the
// results, results, users). The search's meaning was Live's: the procedure named a
// query on its servers, so what is asked for here is what the title passes, and each
// must match. The title sends two procedures, which it fills in alike. The numbers that
// are no integers are skill bounds, which nothing here keeps, and the room for a party is
// a lower bound.
fn xgi_session_search(memory: Memory, buffer: u32, length: u32, overlapped: u32) -> Reply {
    if buffer == 0 || length < 36 {
        return result(ERROR_INVALID_PARAMETER);
    }
    let words = words(memory, buffer, 8);
    // The title's wrapper has cleared the header: nothing found.
    let Some(service) = online::get().filter(|service| service.matchmaking()) else { return DONE };
    if overlapped == 0 {
        return DONE;
    }

    let mut search = SessionSearch { room: 0, most: words[2], wants: Vec::new() };
    let counts = words[3];
    let contexts = words[5];
    if contexts != 0 {
        for i in 0..(counts & 0xFFFF) {
            let at = contexts.wrapping_add(8 * i);
            search.wants.push(SessionWant {
                id: memory.u32_at(at),
                value: memory.u32_at(at.wrapping_add(4)),
                at_least: false,
            });
        }
    }
    // XUSER_PROPERTY { id, pad, XUSER_DATA { type at 8, value at 16 } }
    let properties = words[4];
    if properties != 0 {
        for i in 0..(counts >> 16) {
            let property = properties.wrapping_add(24 * i);
            if memory.u8_at(property.wrapping_add(8)) != 1 {
                continue;
            }
            let (id, value) = (memory.u32_at(property), memory.u32_at(property.wrapping_add(16)));
            search.wants.push(SessionWant { id, value, at_least: id == PARTY_ROOM });
            if id == PARTY_ROOM {
                search.room = value;
            }
        }
    }
    net::forget_session_lags();
    // The title polls this block, and takes anything but pending as the answer, in the
    // frame after asking.
    memory.store_u32(overlapped, ERROR_IO_PENDING);
    let id = service.search(&search);
    lock(&SEARCHES).push(Search { id, overlapped, results: words[7], capacity: words[6], most: words[2] });
    log_info!("online: searching for sessions, procedure %u, room for %u", words[0], search.room);
    Reply { pending: true, ..DONE }
}

// XSESSION_SEARCHRESULT_HEADER, then the results: { XSESSION_INFO, open public, open
// private, filled public, filled private, properties, contexts, their pointers }. The
// title reads no property a search result can leave out. The host's address is rebuilt
// from the id the service gives for the listing: a relay stamps it, so nobody can list a
// session in another player's name there. A Steam listing says it itself.
fn answer_search(memory: Memory, search: &Search, found: &[SessionFound]) {
    let mut count = 0u32;
    if search.results != 0 && search.capacity >= 8 {
        let first = search.results.wrapping_add(8);
        for one in found {
            if count >= search.most || 8 + (count + 1) * FINDING_SIZE > search.capacity {
                break;
            }
            let at = first.wrapping_add(count * FINDING_SIZE);
            let mut info = one.session;
            let mut address = host_address(&info);
            net::set_peer(&mut address, one.host);
            info[HOST_ADDRESS..KEY].copy_from_slice(&address);
            memory.zero(at, FINDING_SIZE);
            memory.store_bytes(at, &info);
            let slots = at.wrapping_add(SESSION_INFO_SIZE as u32);
            for (i, slot) in (0u32..).zip([one.open_public, one.open_private, one.filled_public, one.filled_private]) {
                memory.store_u32(slots.wrapping_add(4 * i), slot);
            }
            net::note_session_lag(session_id(&one.session), one.lag);
            count += 1;
        }
        memory.store_u32(search.results, count);
        memory.store_u32(search.results.wrapping_add(4), first);
    }
    log_info!("online: search found %u sessions", count);
    complete_overlapped_ex(memory, search.overlapped, ERROR_SUCCESS, ERROR_SUCCESS, 0);
}

/// Once a frame: answers the searches whose results have come.
pub fn finish_searches() {
    let Some(service) = online::get() else { return };
    let memory = crate::runtime::guest_memory::memory();
    let mut searches = lock(&SEARCHES);
    searches.retain(|search| {
        let mut found = Vec::new();
        if !service.search_results(search.id, &mut found) {
            return true;
        }
        answer_search(memory, search, &found);
        false
    });
}

// (session, flags, publicSlots, privateSlots)
fn xgi_session_modify(memory: Memory, buffer: u32, length: u32) -> Reply {
    if buffer == 0 || length < 16 {
        return result(ERROR_INVALID_PARAMETER);
    }
    let words = words(memory, buffer, 4);
    if let Some(session) = lock(&SESSIONS).by_object.get_mut(&words[0]) {
        // The flags that say who hosts and what the session uses do not change here; the
        // rest can.
        const FIXED: u32 = 0x3F;
        session.flags = (session.flags & FIXED) | (words[1] & !FIXED);
        session.public_slots = words[2];
        session.private_slots = words[3];
    }
    publish_hosted(); // the slots are in the connect line
    publish_joinable();
    publish_listing();
    DONE
}

// ---- Live storage -----------------------------------------------------------------

// Live storage, kept on this machine under saves/online: the title's own files (title/,
// which nothing here publishes, so each is there and empty -- the playlists and the
// message of the day), and each player's (user/<xuid>/, the multiplayer rank and stats the
// title uploads).
//
// The console names them by server path, which XStorageBuildServerPath makes and the
// title passes back: //title.<id>/t:<id>/<file> and //tuser.<id>/u:<xuid>/<id>/<file>.
const FACILITY_PER_TITLE: u32 = 2;
const FACILITY_PER_USER_TITLE: u32 = 3;
const X_ONLINE_E_STORAGE_FILE_NOT_FOUND: u32 = 0x8015_C004;

struct StorageFile {
    path: PathBuf,
    name: String,
    title: bool,
}

fn storage_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':'])
}

fn storage_path(server: &str) -> Option<StorageFile> {
    let title = format!("{TITLE_ID:08x}");
    let root = save_root().join("online");
    if let Some(name) = server.strip_prefix(&format!("//title.{title}/t:{title}/")) {
        return storage_name(name).then(|| StorageFile {
            path: root.join("title").join(name),
            name: name.to_owned(),
            title: true,
        });
    }
    // <xuid>/<title>/<file>
    let rest = server.strip_prefix(&format!("//tuser.{title}/u:"))?;
    let (xuid, rest) = rest.split_once('/')?;
    let (named, name) = rest.split_once('/')?;
    if xuid.len() != 16 || named != title || !xuid.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    storage_name(name).then(|| StorageFile {
        path: root.join("user").join(xuid).join(name),
        name: name.to_owned(),
        title: false,
    })
}

// Wide text as ASCII, '?' for what is not.
fn wide_to_ascii(memory: Memory, address: u32, count: u32) -> String {
    (0..count)
        .map(|i| memory.u16_at(address.wrapping_add(2 * i)))
        .take_while(|&c| c != 0)
        .map(|c| if c < 0x80 { char::from(c as u8) } else { '?' })
        .collect()
}

// Wide text as UTF-8, a character at a time, as GuestWideToUtf8 has it.
fn wide_to_utf8(memory: Memory, address: u32, most: u32) -> Vec<u8> {
    let mut out = Vec::new();
    if address == 0 {
        return out;
    }
    for c in (0..most).map(|i| memory.u16_at(address.wrapping_add(2 * i))).take_while(|&c| c != 0) {
        if c < 0x80 {
            out.push(c as u8);
        } else if c < 0x800 {
            out.extend([0xC0 | (c >> 6) as u8, 0x80 | (c & 0x3F) as u8]);
        } else {
            out.extend([0xE0 | (c >> 12) as u8, 0x80 | ((c >> 6) & 0x3F) as u8, 0x80 | (c & 0x3F) as u8]);
        }
    }
    out
}

// (XSTORAGE_BUILD_SERVER_PATH*): { user, xuid@8, facility@16, info@20, infoSize@24,
// fileName@28, pathOut@32, pathLengthInOut@36 }
fn storage_build_server_path(memory: Memory, buffer: u32) -> Reply {
    if buffer == 0 {
        return result(E_INVALIDARG);
    }
    let user = memory.u32_at(buffer);
    let named = memory.u64_at(buffer.wrapping_add(8));
    let facility = memory.u32_at(buffer.wrapping_add(16));
    let file_name = memory.u32_at(buffer.wrapping_add(28));
    let path_out = memory.u32_at(buffer.wrapping_add(32));
    let length = memory.u32_at(buffer.wrapping_add(36));
    let name = wide_to_utf8(memory, file_name, 256);
    if length == 0 || name.is_empty() {
        return result(E_INVALIDARG);
    }

    let mut path = match facility {
        FACILITY_PER_TITLE => format!("//title.{TITLE_ID:08x}/t:{TITLE_ID:08x}/").into_bytes(),
        FACILITY_PER_USER_TITLE => {
            let xuid = if user == LOCAL_USER { local_online_xuid() } else { named };
            format!("//tuser.{TITLE_ID:08x}/u:{xuid:016x}/{TITLE_ID:08x}/").into_bytes()
        }
        _ => return result(0x8015_5301), // X_ONLINE_E_STORAGE_INVALID_FACILITY
    };
    path.extend_from_slice(&name);
    path.truncate(255); // the console's buffer

    let chars = path.len() as u32 + 1;
    if path_out != 0 {
        let room = memory.u32_at(length).min(chars);
        for i in 0..room {
            let c = if i + 1 < chars { u16::from(path[i as usize]) } else { 0 };
            memory.store_u16(path_out.wrapping_add(2 * i), c);
        }
    }
    memory.store_u32(length, chars);
    DONE
}

// XLIVEBASE_ASYNC_MESSAGE { task }, and the task's marshalled request: { user,
// pathChars, path (UTF-16), bufferSize, buffer }.
#[derive(Default)]
struct StorageRequest {
    path: String,
    size: u32,
    buffer: u32,
    results: u32,
    results_size: u32,
}

fn read_storage_request(memory: Memory, message: u32, out: &mut StorageRequest) -> bool {
    let task = if message != 0 { memory.u32_at(message) } else { 0 };
    if task == 0 {
        return false;
    }
    let request = memory.u32_at(task.wrapping_add(0x18));
    let request_size = memory.u32_at(task.wrapping_add(0x1C));
    out.results = memory.u32_at(task.wrapping_add(0x2C));
    out.results_size = memory.u32_at(task.wrapping_add(0x30));
    if request == 0 || request_size < 16 {
        return false;
    }
    let chars = memory.u32_at(request.wrapping_add(4));
    if 8 + u64::from(chars) * 2 + 8 > u64::from(request_size) {
        return false;
    }
    out.path = wide_to_ascii(memory, request.wrapping_add(8), chars);
    let tail = request.wrapping_add(8).wrapping_add(chars * 2);
    out.size = memory.u32_at(tail);
    out.buffer = memory.u32_at(tail.wrapping_add(4));
    true
}

fn text(text: &str) -> std::ffi::CString {
    c_string(text.as_bytes())
}

// XSTORAGE_DOWNLOAD_TO_MEMORY_RESULTS { bytesTotal, xuidOwner, ftCreated }
fn storage_download(memory: Memory, message: u32) -> Reply {
    let mut request = StorageRequest::default();
    let file = if read_storage_request(memory, message, &mut request) { storage_path(&request.path) } else { None };
    let Some(file) = file else {
        log_warn!("online: storage download of an unknown path \"%s\"", text(&request.path).as_c_str());
        return Reply { result: ERROR_FUNCTION_FAILED, extended: X_ONLINE_E_STORAGE_FILE_NOT_FOUND, pending: false };
    };
    let mut shared = Vec::new();
    let mut bytes = Vec::new();
    if file.title && online::get().is_some_and(|service| service.title_file(file.name.as_bytes(), &mut shared)) {
        bytes = shared;
    } else {
        if file.path.exists() {
            bytes = fs::read(&file.path).unwrap_or_default();
        } else if !file.title {
            log_info!("online: storage has no %s yet", text(&request.path).as_c_str());
            return Reply {
                result: ERROR_FUNCTION_FAILED,
                extended: X_ONLINE_E_STORAGE_FILE_NOT_FOUND,
                pending: false,
            };
        }
    }
    if bytes.len() > request.size as usize {
        log_warn!(
            "online: %s has %llu bytes, and the title has room for %u",
            text(&request.path).as_c_str(),
            bytes.len() as u64,
            request.size
        );
        // HRESULT_FROM_WIN32(ERROR_INSUFFICIENT_BUFFER)
        return Reply { result: ERROR_FUNCTION_FAILED, extended: 0x8007_007A, pending: false };
    }
    if request.buffer != 0 && !bytes.is_empty() {
        memory.store_bytes(request.buffer, &bytes);
    }
    if request.results != 0 && request.results_size >= 20 {
        memory.zero(request.results, 20);
        memory.store_u32(request.results, bytes.len() as u32);
        memory.store_u64(request.results.wrapping_add(4), if file.title { 0 } else { local_online_xuid() });
    }
    log_info!("online: storage download %s, %llu bytes", text(&request.path).as_c_str(), bytes.len() as u64);
    DONE
}

fn storage_upload(memory: Memory, message: u32) -> Reply {
    let mut request = StorageRequest::default();
    let file = if read_storage_request(memory, message, &mut request) { storage_path(&request.path) } else { None };
    let Some(file) = file.filter(|file| !file.title && request.buffer != 0) else {
        log_warn!("online: storage upload refused for \"%s\"", text(&request.path).as_c_str());
        return result(0x8007_0005); // E_ACCESSDENIED
    };
    if let Some(folder) = file.path.parent() {
        let _ = fs::create_dir_all(folder);
    }
    // From guest memory a piece at a time, as the title's size is the title's.
    let written = fs::File::create(&file.path).and_then(|mut out| {
        for at in (0..request.size).step_by(0x10000) {
            let piece = (request.size - at).min(0x10000) as usize;
            out.write_all(&memory.bytes(request.buffer.wrapping_add(at), piece))?;
        }
        Ok(())
    });
    if written.is_err() {
        return result(0x8000_4005); // E_FAIL
    }
    log_info!("online: storage upload %s, %u bytes", text(&request.path).as_c_str(), request.size);
    DONE
}

// ---- invitations ------------------------------------------------------------------

// The invitation the player last accepted, as XINVITE_INFO: { xuidInvitee, xuidInviter,
// titleId, hostInfo (XSESSION_INFO), fromGameInvite }.
const INVITE_SIZE: usize = 84;
static INVITE: Mutex<Option<[u8; INVITE_SIZE]>> = Mutex::new(None);

/// The player accepted an invitation, or chose to join a friend: the title is told, now
/// or when it first listens for Live.
pub fn accept_invite(inviter_account: u64, session: &SessionInfo, from_invite: bool) {
    let mut invite = [0; INVITE_SIZE];
    invite[0..8].copy_from_slice(&local_online_xuid().to_be_bytes());
    invite[8..16].copy_from_slice(&online_xuid(inviter_account).to_be_bytes());
    invite[16..20].copy_from_slice(&TITLE_ID.to_be_bytes());
    invite[20..80].copy_from_slice(session);
    invite[80..84].copy_from_slice(&u32::from(from_invite).to_be_bytes());
    *lock(&INVITE) = Some(invite);
    let mut listeners = lock(&LISTENERS);
    listeners.invite_unheard = !post_held(&listeners, XN_LIVE_INVITE_ACCEPTED, LOCAL_USER);
}

// (task, X_ARGUMENT_LIST*): entries of { size, value } where value 0 points at the user
// index and value 1 at the XINVITE_INFO to fill.
fn invite_get_accepted_info(memory: Memory, arguments: u32) -> Reply {
    if arguments == 0 {
        return result(E_INVALIDARG);
    }
    let user = memory.u64_at(arguments.wrapping_add(8)) as u32;
    let info = memory.u64_at(arguments.wrapping_add(24)) as u32;
    if user == 0 || info == 0 || memory.u32_at(user) != LOCAL_USER {
        return result(E_INVALIDARG);
    }
    let Some(invite) = *lock(&INVITE) else { return result(0x8015_5200) }; // X_ONLINE_E_SESSION_NOT_FOUND
    memory.store_bytes(info, &invite);
    log_info!("xam: the title read the accepted invitation");
    DONE
}

fn xlivebase(memory: Memory, message: u32, param1: u32, param2: u32) -> Reply {
    let store = |at: u32, value: u32| {
        if at != 0 {
            memory.store_u32(at, value);
        }
    };
    match message {
        0x0005_0009 => storage_download(memory, param1),
        0x0005_000B => storage_upload(memory, param1),
        0x0005_8004 => {
            store(param1, 1); // XOnlineGetLogonID
            DONE
        }
        0x0005_8006 => {
            store(param1, 1); // XOnlineGetNatType: open
            DONE
        }
        0x0005_800E => {
            store(param2, 0); // XUserMuteListQuery: nobody is muted
            DONE
        }
        0x0005_8023 => invite_get_accepted_info(memory, param2),
        0x0005_8035 => storage_build_server_path(memory, param1),
        _ => DONE,
    }
}

// XUserSetContext: (user index, unused, XUID, context id, value). The player is the
// XUID's when there is one, as Xenia reads it.
fn xgi_user_set_context(memory: Memory, buffer: u32) -> Reply {
    if buffer == 0 {
        return result(E_INVALIDARG);
    }
    let set = words(memory, buffer, 6);
    let mut user = set[0];
    let xuid = (u64::from(set[2]) << 32) | u64::from(set[3]);
    if xuid != 0 {
        for index in 0..4 {
            let who = who_is(index);
            if who.state != 0 && (who.offline == xuid || who.online == xuid) {
                user = index;
            }
        }
    }
    presence::set_context(user, set[4], set[5]);
    note_searched(set[4], set[5]);
    DONE
}

fn handled(app: u32, message: u32) -> bool {
    match app {
        APP_XGI => matches!(
            message,
            0x000B_0006
                | 0x000B_0007
                | 0x000B_0010
                | 0x000B_0011
                | 0x000B_0012
                | 0x000B_0013
                | 0x000B_0014
                | 0x000B_0015
                | 0x000B_0018
                | 0x000B_001C
                | 0x000B_001E
                | 0x000B_001F
        ),
        APP_XLIVEBASE => matches!(
            message,
            0x0005_0009 | 0x0005_000B | 0x0005_8004 | 0x0005_8006 | 0x0005_800E | 0x0005_8023 | 0x0005_8035
        ),
        _ => false,
    }
}

static UNHANDLED: Mutex<BTreeSet<u64>> = Mutex::new(BTreeSet::new());

fn xam_message(memory: Memory, app: u32, message: u32, param1: u32, param2: u32, overlapped: u32) -> Reply {
    if !handled(app, message) {
        // Answered "success" with nothing written; named once so a run shows what the
        // title asked for.
        if lock(&UNHANDLED).insert((u64::from(app) << 32) | u64::from(message)) {
            log_info!("xam: message %02x/%08x answered with success", app, message);
        }
        return DONE;
    }
    if app != APP_XGI {
        return xlivebase(memory, message, param1, param2);
    }
    match message {
        0x000B_0006 => xgi_user_set_context(memory, param1),
        0x000B_0007 => xgi_user_set_property(memory, param1),
        0x000B_0010 => xgi_session_create(memory, param1, param2),
        0x000B_0011 => xgi_session_delete(memory, param1),
        0x000B_0012 => xgi_session_join_remote(memory, param1, param2),
        0x000B_0013 => xgi_session_leave_remote(memory, param1, param2),
        0x000B_0014 => xgi_session_start(memory, param1),
        0x000B_0015 => xgi_session_end(memory, param1),
        0x000B_0018 => xgi_session_modify(memory, param1, param2),
        0x000B_001C => xgi_session_search(memory, param1, param2, overlapped),
        0x000B_001E => xgi_session_migrate_host(memory, param1, param2),
        _ => DONE, // XSessionModifySkill: no service keeps a skill
    }
}

/// A player's rank, Live storage and settings, kept under one profile number, move to
/// another: the sign-in screen gave the profile a new one.
pub fn move_player_data(from: u64, to: u64) {
    #[cfg(mw2_screens_rig)]
    return screens_rig(c"move", 0, 0);
    #[allow(unreachable_code)]
    let (from, to) = (from & ACCOUNT_BITS, to & ACCOUNT_BITS);
    if from == 0 || from == to {
        return;
    }
    let saves = save_root();
    let was = format!("mpdata_{}", xuid_text(OFFLINE_XUID | from));
    let is = format!("mpdata_{}", xuid_text(OFFLINE_XUID | to));
    if saves.join(&was).join(&was).exists() && !saves.join(&is).exists() {
        let moved = fs::rename(saves.join(&was), saves.join(&is))
            .and_then(|()| fs::rename(saves.join(&is).join(&was), saves.join(&is).join(&is)));
        log_info!(
            "xam: the rank kept as %s is now %s%s",
            text(&was).as_c_str(),
            text(&is).as_c_str(),
            if moved.is_ok() { c"" } else { c" -- not moved" }
        );
    }
    let users = saves.join("online").join("user");
    let (live_was, live_is) = (users.join(xuid_text(online_xuid(from))), users.join(xuid_text(online_xuid(to))));
    if live_was.is_dir() && !live_is.exists() {
        let _ = fs::rename(live_was, live_is);
    }
    // A profile's settings at the second to fourth controllers (profile.rs).
    let (settings_was, settings_is) =
        (saves.join(format!("profile_{from:012x}.bin")), saves.join(format!("profile_{to:012x}.bin")));
    if settings_was.exists() && !settings_is.exists() {
        let _ = fs::rename(settings_was, settings_is);
    }
}

// A session is a kernel object the title names by handle and hands XAM by object address
// in every session message. Nothing here reads the object; it only has to be memory of
// its own.
struct SessionObject {
    guest: u32,
}

fn session_object_of(object: &Object) -> Option<&SessionObject> {
    match &object.kind {
        Kind::Other(other) => other.downcast_ref::<SessionObject>(),
        _ => None,
    }
}

// (handleOut)
kernel_import!(XamSessionCreateHandle, |ctx, memory| {
    let guest = allocate_guest(64, 16);
    let handle = if guest != 0 {
        memory.zero(guest, 64);
        objects::lock().insert(Object::new(Kind::Other(Box::new(SessionObject { guest }))))
    } else {
        0
    };
    let out = ctx.r3.as_u32();
    if out != 0 {
        memory.store_u32(out, handle);
    }
    ctx.r3.u64 = u64::from(if handle != 0 { ERROR_SUCCESS } else { ERROR_NOT_ENOUGH_MEMORY });
});

// (handle, objectOut)
kernel_import!(XamSessionRefObjByHandle, |ctx, memory| {
    let object = objects::lock().lookup(ctx.r3.as_u32()).cloned();
    let guest = object.as_deref().and_then(session_object_of).map(|session| session.guest);
    let out = ctx.r4.as_u32();
    if out != 0 {
        memory.store_u32(out, guest.unwrap_or(0));
    }
    ctx.r3.u64 = u64::from(if guest.is_some() { ERROR_SUCCESS } else { ERROR_INVALID_HANDLE });
});

// (app, message, param1, param2)
kernel_import!(XMsgInProcessCall, |ctx, memory| {
    let reply = xam_message(memory, ctx.r3.as_u32(), ctx.r4.as_u32(), ctx.r5.as_u32(), ctx.r6.as_u32(), 0);
    ctx.r3.u64 = u64::from(reply.result);
});

// (app, message, overlapped, buffer, length). The asynchronous form of the same call: it
// finishes inline here, but the caller polls the overlapped block, so the answer has to
// go there and the call itself say "pending".
kernel_import!(XMsgStartIORequest, |ctx, memory| {
    let overlapped = ctx.r5.as_u32();
    let reply = xam_message(memory, ctx.r3.as_u32(), ctx.r4.as_u32(), ctx.r6.as_u32(), ctx.r7.as_u32(), overlapped);
    if reply.pending {
        ctx.r3.u64 = u64::from(ERROR_IO_PENDING);
        return;
    }
    if overlapped == 0 {
        ctx.r3.u64 = u64::from(reply.result);
        return;
    }
    complete_overlapped_ex(memory, overlapped, reply.result, reply.extended, 0);
    ctx.r3.u64 = u64::from(ERROR_IO_PENDING);
});

// (overlapped, wait). The only call that can still be running is a search; the title may
// reuse its block once this returns, so its answer must not come.
kernel_import!(XMsgCancelIORequest, |ctx, memory| {
    let overlapped = ctx.r3.as_u32();
    let mut searches = lock(&SEARCHES);
    if let Some(at) = searches.iter().position(|search| search.overlapped == overlapped) {
        searches.remove(at);
        complete_overlapped_ex(memory, overlapped, ERROR_CANCELLED, 0x8007_0000 | ERROR_CANCELLED, 0);
    }
    drop(searches);
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

// ---- enumerators ------------------------------------------------------------------

// Enumerators: the title asks XAM for a list -- content packages, Live's title servers,
// friends -- by a handle it then pages through. Nothing here has any of those to list,
// so every enumeration ends at once, which is what the console answers for a player with
// no downloaded content and a Live with no servers for the title.
struct Enumerator {
    /// the caller's private block, XamCreateEnumeratorHandle's
    extra: u32,
}

impl Drop for Enumerator {
    fn drop(&mut self) {
        if self.extra != 0 {
            free_guest(self.extra);
        }
    }
}

fn enumerator_of(object: &Object) -> Option<&Enumerator> {
    match &object.kind {
        Kind::Other(other) => other.downcast_ref::<Enumerator>(),
        _ => None,
    }
}

fn new_enumerator(memory: Memory, extra_size: u32) -> u32 {
    let mut extra = 0;
    if extra_size != 0 {
        extra = allocate_guest(extra_size, 16);
        if extra == 0 {
            return 0;
        }
        memory.zero(extra, extra_size);
    }
    objects::lock().insert(Object::new(Kind::Other(Box::new(Enumerator { extra }))))
}

const XCONTENT_DATA_SIZE: u32 = 0x134;

// (user, app, openMessage, closeMessage, extraSize, itemCount, flags, handleOut)
kernel_import!(XamCreateEnumeratorHandle, |ctx, memory| {
    let handle = new_enumerator(memory, ctx.r7.as_u32());
    let out = ctx.r10.as_u32();
    if out != 0 {
        memory.store_u32(out, handle);
    }
    ctx.r3.u64 = u64::from(if handle != 0 { ERROR_SUCCESS } else { ERROR_NOT_ENOUGH_MEMORY });
});

// (handle, extraOut)
kernel_import!(XamGetPrivateEnumStructureFromHandle, |ctx, memory| {
    let object = objects::lock().lookup(ctx.r3.as_u32()).cloned();
    let extra = object.as_deref().and_then(enumerator_of).map(|enumerator| enumerator.extra);
    let out = ctx.r4.as_u32();
    if out != 0 {
        memory.store_u32(out, extra.unwrap_or(0));
    }
    ctx.r3.u64 = u64::from(if extra.is_some() { ERROR_SUCCESS } else { ERROR_INVALID_HANDLE });
});

// (user, device, contentType, flags, itemCount, bufferSizeOut, handleOut)
kernel_import!(XamContentCreateEnumerator, |ctx, memory| {
    let handle = new_enumerator(memory, 0);
    let (size, out) = (ctx.r8.as_u32(), ctx.r9.as_u32());
    if size != 0 {
        memory.store_u32(size, ctx.r7.as_u32().wrapping_mul(XCONTENT_DATA_SIZE));
    }
    if out != 0 {
        memory.store_u32(out, handle);
    }
    ctx.r3.u64 = u64::from(if handle != 0 { ERROR_SUCCESS } else { ERROR_NOT_ENOUGH_MEMORY });
});

// (handle, flags, buffer, bufferSize, itemsOut, overlapped)
kernel_import!(XamEnumerate, |ctx, memory| {
    let object = objects::lock().lookup(ctx.r3.as_u32()).cloned();
    if object.as_deref().and_then(enumerator_of).is_none() {
        ctx.r3.u64 = u64::from(ERROR_INVALID_HANDLE);
        return;
    }
    let overlapped = ctx.r8.as_u32();
    if overlapped != 0 {
        complete_overlapped_ex(memory, overlapped, ERROR_FUNCTION_FAILED, 0x8007_0000 | ERROR_NO_MORE_FILES, 0);
        ctx.r3.u64 = u64::from(ERROR_IO_PENDING);
        return;
    }
    let items = ctx.r7.as_u32();
    if items != 0 {
        memory.store_u32(items, 0);
    }
    ctx.r3.u64 = u64::from(ERROR_NO_MORE_FILES);
});

// XamGetSystemVersion, XGetLanguage and XGetAVPack are in misc.rs.

// (flags, size, outPointer)
kernel_import!(XamAlloc, |ctx, memory| {
    let address = allocate_guest(ctx.r4.as_u32(), 16);
    let out = ctx.r5.as_u32();
    if out != 0 {
        memory.store_u32(out, address);
    }
    ctx.r3.u64 = u64::from(if address != 0 { ERROR_SUCCESS } else { STATUS_NO_MEMORY });
});

kernel_import!(XamFree, |ctx, _memory| {
    free_guest(ctx.r3.as_u32());
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

// (category, setting, buffer, size, sizeOut)
kernel_import!(ExGetXConfigSetting, |ctx, memory| {
    let (category, setting) = (ctx.r3.as_u32(), ctx.r4.as_u32());
    let (buffer, size) = (ctx.r5.as_u32(), ctx.r6.as_u32());
    let (value, width) = match (category, setting) {
        (0x0003, 0x0001) => (0, 1), // XCONFIG_USER_CATEGORY: timezone-ish
        (0x0003, 0x0002) => (1, 4), // language: English
        _ => (0, 4),                // video flags, and the rest
    };
    if buffer != 0 && size >= width {
        if width == 1 {
            memory.store_u8(buffer, value as u8);
        } else {
            memory.store_u32(buffer, value);
        }
    }
    let out = ctx.r7.as_u32();
    if out != 0 {
        memory.store_u16(out, width as u16);
    }
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});
