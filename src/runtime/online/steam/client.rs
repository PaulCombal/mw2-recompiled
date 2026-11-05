// The Steam client, called the way libsteam_api calls it, with nothing of Valve's
// built in or shipped.
//
// libsteam_api is a thin layer over steamclient.so, the library every Steam install
// carries: it opens it, asks it for versioned interfaces, and calls them. This does
// the same. The interfaces are C++ objects, called here through their vtables by
// slot; the slots and the structures below are those of the Steamworks SDK's headers
// (the versions named with each), and Steam keeps every version it ever shipped, so a
// build keeps working as the client updates. Everything Steam hands back is copied at
// once: a string it returns may be held in a buffer its next call reuses.

use crate::env;
use crate::log::{c_string, log_error, log_info};
use crate::runtime::platform;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::PathBuf;
use std::ptr;

type HSteamPipe = i32;
type HSteamUser = i32;
pub type Call = u64;

// Slots, from steam_api.json and the headers of the SDK version named.
mod steam_client {
    // SteamClient023
    pub const CREATE_STEAM_PIPE: usize = 0;
    pub const RELEASE_STEAM_PIPE: usize = 1;
    pub const CONNECT_TO_GLOBAL_USER: usize = 2;
    pub const RELEASE_USER: usize = 4;
    pub const GET_USER: usize = 5;
    pub const GET_FRIENDS: usize = 8;
    pub const GET_UTILS: usize = 9;
    pub const GET_MATCHMAKING: usize = 10;
    pub const GET_GENERIC: usize = 12;
}
mod user {
    // SteamUser023
    pub const LOGGED_ON: usize = 1;
    pub const GET_STEAM_ID: usize = 2;
}
mod friends {
    // SteamFriends018
    pub const GET_PERSONA_NAME: usize = 0;
    pub const COUNT: usize = 2;
    pub const BY_INDEX: usize = 3;
    pub const PERSONA_STATE: usize = 5;
    pub const NAME: usize = 6;
    pub const GAME_PLAYED: usize = 7;
    pub const MEDIUM_AVATAR: usize = 34;
    pub const SET_RICH_PRESENCE: usize = 41;
    pub const RICH_PRESENCE: usize = 43;
}
mod utils {
    // SteamUtils011
    pub const IMAGE_SIZE: usize = 5;
    pub const IMAGE_RGBA: usize = 6;
}
mod matchmaking {
    // SteamMatchMaking009
    pub const REQUEST_LOBBY_LIST: usize = 4;
    pub const ADD_STRING_FILTER: usize = 5;
    pub const ADD_DISTANCE_FILTER: usize = 9;
    pub const RESULT_COUNT: usize = 10;
    pub const LOBBY_BY_INDEX: usize = 12;
    pub const CREATE_LOBBY: usize = 13;
    pub const JOIN_LOBBY: usize = 14;
    pub const LEAVE_LOBBY: usize = 15;
    pub const INVITE_TO_LOBBY: usize = 16;
    pub const MEMBER_COUNT: usize = 17;
    pub const MEMBER: usize = 18;
    pub const GET_LOBBY_DATA: usize = 19;
    pub const SET_LOBBY_DATA: usize = 20;
    pub const SET_JOINABLE: usize = 34;
    pub const OWNER: usize = 35;
    pub const SET_OWNER: usize = 36;
}
mod messages {
    // SteamNetworkingMessages002
    pub const SEND: usize = 0;
    pub const RECEIVE: usize = 1;
    pub const ACCEPT: usize = 2;
    pub const SESSION_INFO: usize = 5;
}

// Callback ids: the interface's base plus the structure's offset.
const CALL_COMPLETED: i32 = 703; // SteamAPICallCompleted_t
pub const LOBBY_ENTER: i32 = 504; // LobbyEnter_t
const LOBBY_DATA_UPDATE: i32 = 505; // LobbyDataUpdate_t
pub const LOBBY_CREATED: i32 = 513; // LobbyCreated_t
pub const LOBBY_MATCH_LIST: i32 = 510; // LobbyMatchList_t
const LOBBY_JOIN_REQUESTED: i32 = 333; // GameLobbyJoinRequested_t
const PRESENCE_JOIN_REQUESTED: i32 = 337; // GameRichPresenceJoinRequested_t
const SESSION_REQUEST: i32 = 1251; // SteamNetworkingMessagesSessionRequest_t

/// SteamNetworkingIdentity, packed to 1 byte as the SDK declares it: a type, 16 for a
/// SteamID, a size, and 128 bytes whose first 8 are the SteamID.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct Identity {
    kind: i32,
    size: i32,
    raw: [u8; 128],
}
const _: () = assert!(size_of::<Identity>() == 136);

impl Identity {
    pub fn of(steam_id: u64) -> Self {
        let mut raw = [0u8; 128];
        raw[..8].copy_from_slice(&steam_id.to_ne_bytes());
        Identity { kind: 16, size: 8, raw }
    }

    pub fn steam_id(&self) -> Option<u64> {
        (self.kind == 16).then(|| u64::from_ne_bytes(self.raw[..8].try_into().unwrap()))
    }

    /// The 136 bytes at `at`, which must be that many.
    unsafe fn read(at: *const u8) -> Self {
        unsafe { ptr::read_unaligned(at.cast()) }
    }
}

/// What a callback says, of the ones read here.
pub enum Callback {
    CallCompleted(Call),
    LobbyJoinRequested(u64),
    PresenceJoinRequested(Vec<u8>),
    LobbyDataUpdate { lobby: u64, member: u64 },
    SessionRequest(Identity),
    Other,
}

/// What a lobby list, a lobby made or a lobby entered answered.
pub struct LobbyCreated {
    pub result: i32,
    pub lobby: u64,
}

pub struct LobbyEnter {
    pub lobby: u64,
    pub response: u32,
}

/// SteamNetConnectionInfo_t's and SteamNetConnectionRealTimeStatus_t's fields read
/// here, laid out alike packed to 4 and to 8.
#[derive(Default)]
pub struct LinkStatus {
    pub ping: i32,
    /// the share of packets that arrive, here and there; -1 until Steam has a measure
    pub quality_local: f32,
    pub quality_remote: f32,
    pub out_bytes: f32,
    pub in_bytes: f32,
    pub send_rate: i32,
    pub queue_microseconds: i64,
    pub end_reason: i32,
    pub end_debug: Vec<u8>,
}

const LINK_INFO: usize = 696;
const LINK_STATUS: usize = 120;

/// What `read` may make of bytes: numbers, and a pointer as the bytes give it.
trait Plain: Copy {}
impl Plain for i32 {}
impl Plain for u32 {}
impl Plain for i64 {}
impl Plain for u64 {}
impl Plain for f32 {}
impl Plain for *const u8 {}

fn read<T: Plain>(bytes: &[u8], at: usize) -> T {
    assert!(at + size_of::<T>() <= bytes.len());
    unsafe { ptr::read_unaligned(bytes.as_ptr().add(at).cast()) }
}

// SteamNetworkingMessage_t's fields read here: packed to 8 on Windows, to 4 elsewhere,
// which moves none of these.
const MESSAGE_DATA: usize = 0;
const MESSAGE_SIZE: usize = 8;
const MESSAGE_PEER: usize = 16;
const MESSAGE_RELEASE: usize = 184;
// The callback structures, packed as the SDK packs them: to 8 on Windows, to 4
// elsewhere. Of the ones read here only LobbyCreated's lobby moves.
#[cfg(windows)]
const LOBBY_CREATED_LOBBY: usize = 8;
#[cfg(not(windows))]
const LOBBY_CREATED_LOBBY: usize = 4;

/// One call through a vtable: the function at `slot` of the object's.
unsafe fn method<F: Copy>(object: *mut c_void, slot: usize) -> F {
    assert_eq!(size_of::<F>(), size_of::<usize>());
    unsafe {
        let table = *object.cast::<*const usize>();
        std::mem::transmute_copy(&*table.add(slot))
    }
}

type CreateInterface = unsafe extern "C" fn(*const c_char, *mut c_int) -> *mut c_void;
type GetCallback = unsafe extern "C" fn(HSteamPipe, *mut u8) -> u8;
type FreeLastCallback = unsafe extern "C" fn(HSteamPipe);
type GetCallResult = unsafe extern "C" fn(HSteamPipe, Call, *mut c_void, c_int, c_int, *mut u8) -> u8;

pub struct Client {
    client: *mut c_void,
    steam_user: *mut c_void,
    friends: *mut c_void,
    utils: *mut c_void,
    matchmaking: *mut c_void,
    messages: *mut c_void,
    get_callback: GetCallback,
    free_last_callback: FreeLastCallback,
    get_call_result: GetCallResult,
    pipe: HSteamPipe,
    user: HSteamUser,
}

// The client's interfaces are made to be called from any thread.
unsafe impl Send for Client {}
unsafe impl Sync for Client {}

fn text_of(text: *const c_char) -> Option<Vec<u8>> {
    (!text.is_null()).then(|| unsafe { CStr::from_ptr(text) }.to_bytes().to_vec())
}

/// Both copies of a variable on Windows: the C library's, which getenv reads, and the
/// process's own, which the Steam client writes and SDL reads.
fn set_environment(name: &CStr, value: Option<&CStr>) {
    #[cfg(not(windows))]
    {
        unsafe extern "C" {
            fn setenv(name: *const c_char, value: *const c_char, overwrite: c_int) -> c_int;
            fn unsetenv(name: *const c_char) -> c_int;
        }
        match value {
            Some(value) => unsafe { setenv(name.as_ptr(), value.as_ptr(), 1) },
            None => unsafe { unsetenv(name.as_ptr()) },
        };
    }
    #[cfg(windows)]
    {
        use crate::runtime::windows::SetEnvironmentVariableA;
        unsafe extern "C" {
            fn _putenv_s(name: *const c_char, value: *const c_char) -> c_int;
        }
        unsafe {
            _putenv_s(name.as_ptr(), value.unwrap_or(c"").as_ptr());
            SetEnvironmentVariableA(name.as_ptr(), value.map_or(ptr::null(), CStr::as_ptr));
        }
    }
}

/// The SDL_ variables in the environment, to put back as they were.
struct SdlSettings(Vec<(Vec<u8>, Vec<u8>)>);

impl SdlSettings {
    fn now() -> Self {
        let mut settings = Vec::new();
        let mut keep = |text: &[u8]| {
            if text.starts_with(b"SDL_")
                && let Some(equals) = text.iter().position(|&c| c == b'=')
            {
                settings.push((text[..equals].to_vec(), text[equals + 1..].to_vec()));
            }
        };
        #[cfg(not(windows))]
        {
            unsafe extern "C" {
                static environ: *const *const c_char;
            }
            let mut entry = unsafe { environ };
            while !entry.is_null() && !unsafe { *entry }.is_null() {
                keep(unsafe { CStr::from_ptr(*entry) }.to_bytes());
                entry = unsafe { entry.add(1) };
            }
        }
        #[cfg(windows)]
        {
            use crate::runtime::windows::{FreeEnvironmentStringsA, GetEnvironmentStringsA};
            // The process's own block, not `environ`: the Steam client sets its
            // variables with SetEnvironmentVariable, which the C library's copy never
            // hears of -- and SDL reads this one.
            let block = unsafe { GetEnvironmentStringsA() };
            if !block.is_null() {
                let mut entry = block.cast_const();
                while unsafe { *entry } != 0 {
                    let text = unsafe { CStr::from_ptr(entry) }.to_bytes();
                    keep(text);
                    entry = unsafe { entry.add(text.len() + 1) };
                }
                unsafe { FreeEnvironmentStringsA(block) };
            }
        }
        SdlSettings(settings)
    }

    fn restore(&self) {
        for (name, value) in SdlSettings::now().0 {
            let name_text = c_string(name.clone());
            match self.0.iter().find(|(was, _)| *was == name) {
                None => {
                    log_info!(
                        "online: the Steam client set %s; this was not started by Steam, so it is taken back",
                        name_text.as_c_str()
                    );
                    set_environment(&name_text, None);
                }
                Some((_, was)) if *was != value => set_environment(&name_text, Some(&c_string(was.clone()))),
                Some(_) => {}
            }
        }
    }
}

/// The Steam clients running: processes named steam, each with the install it runs
/// from, two folders above the steamclient.so it has mapped. A client of another
/// install than the running one's half connects, then crashes in its first call.
/// MW2_STEAM_ROOT keeps the one running from it.
#[cfg(not(windows))]
pub struct Running {
    pub pid: i32,
    pub root: PathBuf,
}

#[cfg(not(windows))]
pub fn running_steams() -> Vec<Running> {
    let mut found = Vec::new();
    let named = env::text(c"MW2_STEAM_ROOT");
    let wanted = named.map(|named| std::fs::canonicalize(platform::path(named)).unwrap_or_default());
    let Ok(entries) = std::fs::read_dir("/proc") else { return found };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.is_empty() || !name.bytes().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let comm = std::fs::read(entry.path().join("comm")).unwrap_or_default();
        if comm.split(|&c| c == b'\n').next() != Some(&b"steam"[..]) {
            continue;
        }
        let maps = std::fs::read(entry.path().join("maps")).unwrap_or_default();
        for line in maps.split(|&c| c == b'\n') {
            let Some(slash) = line.iter().position(|&c| c == b'/') else { continue };
            let mapped = line[slash..].strip_suffix(b" (deleted)").unwrap_or(&line[slash..]);
            let library = platform::path(&c_string(mapped));
            if library.file_name() != Some(std::ffi::OsStr::new("steamclient.so")) {
                continue;
            }
            let root = library.parent().and_then(|path| path.parent()).map(std::fs::canonicalize);
            if let Some(Ok(root)) = root
                && wanted.as_ref().is_none_or(|wanted| root == *wanted)
            {
                found.push(Running { pid: name.parse().unwrap_or(0), root });
            }
            break;
        }
    }
    found
}

/// Where the player's Steam keeps its client library: the install the running Steam
/// runs from, else the first of the usual install, the snap and the Flatpak. On
/// Windows, the folder the registry says Steam is installed in. MW2_STEAM_ROOT names
/// the install instead.
fn find_client() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use crate::runtime::windows::{ERROR_SUCCESS, HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
        let root = match env::text(c"MW2_STEAM_ROOT") {
            Some(named) => platform::path(named),
            None => {
                let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
                let mut folder = [0u16; 260];
                let mut bytes = size_of_val(&folder) as u32;
                let found = unsafe {
                    RegGetValueW(
                        HKEY_CURRENT_USER,
                        wide("Software\\Valve\\Steam").as_ptr(),
                        wide("SteamPath").as_ptr(),
                        RRF_RT_REG_SZ,
                        ptr::null_mut(),
                        folder.as_mut_ptr().cast(),
                        &mut bytes,
                    )
                };
                if found != ERROR_SUCCESS {
                    return None;
                }
                let length = folder.iter().position(|&c| c == 0).unwrap_or(folder.len());
                PathBuf::from(String::from_utf16_lossy(&folder[..length]))
            }
        };
        let path = root.join("steamclient64.dll");
        path.exists().then_some(path)
    }
    #[cfg(not(windows))]
    {
        for steam in running_steams() {
            let path = steam.root.join("linux64").join("steamclient.so");
            if path.exists() {
                return Some(path);
            }
        }
        if let Some(named) = env::text(c"MW2_STEAM_ROOT") {
            log_error!("online: no Steam is running from %s", named);
            return None;
        }
        let home = env::text(c"HOME")?;
        const CANDIDATES: [&str; 5] = [
            "/.steam/sdk64/steamclient.so",
            "/.local/share/Steam/linux64/steamclient.so",
            "/.steam/steam/linux64/steamclient.so",
            "/snap/steam/common/.local/share/Steam/linux64/steamclient.so",
            "/.var/app/com.valvesoftware.Steam/.local/share/Steam/linux64/steamclient.so",
        ];
        CANDIDATES
            .iter()
            .map(|candidate| platform::path(&c_string([home.to_bytes(), candidate.as_bytes()].concat())))
            .find(|path| path.exists())
    }
}

fn path_text(path: &std::path::Path) -> CString {
    c_string(path.to_string_lossy().as_bytes())
}

impl Client {
    /// Loads the client and asks it for what this needs; None, with why in the log,
    /// when it cannot. `app` is the game Steam is told this is.
    pub fn open(app: &CStr) -> Option<Client> {
        let Some(path) = find_client() else {
            log_error!("online: no Steam install found (MW2_STEAM_ROOT names one)");
            return None;
        };
        let path_text = path_text(&path);
        // The client reads which game this is from the environment, as it does for a
        // game libsteam_api started, and reads it when it is loaded: set later, its
        // networking asserts and has no interface to give.
        set_environment(c"SteamAppId", Some(app));
        set_environment(c"SteamGameId", Some(app));
        let launched = SdlSettings::now();
        let library = platform::open_library(&path_text);
        if library.is_null() {
            log_error!("online: could not load %s", path_text.as_c_str());
            return None;
        }
        let symbol = |name: &CStr| unsafe { platform::symbol(library, name) };
        let (create, get_callback, free_last_callback, get_call_result) = (
            symbol(c"CreateInterface"),
            symbol(c"Steam_BGetCallback"),
            symbol(c"Steam_FreeLastCallback"),
            symbol(c"Steam_GetAPICallResult"),
        );
        if create.is_null() || get_callback.is_null() || free_last_callback.is_null() || get_call_result.is_null() {
            log_error!("online: %s lacks the exports a client has", path_text.as_c_str());
            return None;
        }
        let create: CreateInterface = unsafe { std::mem::transmute(create) };

        let mut result: c_int = 1;
        let client = unsafe { create(c"SteamClient023".as_ptr(), &mut result) };
        if client.is_null() {
            log_error!("online: the Steam client refused its interface");
            return None;
        }
        let pipe = unsafe {
            method::<unsafe extern "C" fn(*mut c_void) -> HSteamPipe>(client, steam_client::CREATE_STEAM_PIPE)(client)
        };
        let user = if pipe != 0 {
            unsafe {
                method::<unsafe extern "C" fn(*mut c_void, HSteamPipe) -> HSteamUser>(
                    client,
                    steam_client::CONNECT_TO_GLOBAL_USER,
                )(client, pipe)
            }
        } else {
            0
        };
        if user == 0 {
            log_error!("online: Steam is not running, or nobody is logged in");
            unsafe { release(client, pipe, user) };
            return None;
        }

        type GetUser = unsafe extern "C" fn(*mut c_void, HSteamUser, HSteamPipe, *const c_char) -> *mut c_void;
        type GetUtils = unsafe extern "C" fn(*mut c_void, HSteamPipe, *const c_char) -> *mut c_void;
        let get =
            |slot, version: &CStr| unsafe { method::<GetUser>(client, slot)(client, user, pipe, version.as_ptr()) };
        let steam_user = get(steam_client::GET_USER, c"SteamUser023");
        let friends = get(steam_client::GET_FRIENDS, c"SteamFriends018");
        let utils =
            unsafe { method::<GetUtils>(client, steam_client::GET_UTILS)(client, pipe, c"SteamUtils011".as_ptr()) };
        let matchmaking = get(steam_client::GET_MATCHMAKING, c"SteamMatchMaking009");
        let messages = get(steam_client::GET_GENERIC, c"SteamNetworkingMessages002");
        if steam_user.is_null() || friends.is_null() || utils.is_null() || matchmaking.is_null() || messages.is_null() {
            log_error!(
                "online: the Steam client lacks an interface this needs (user %p, friends %p, utils %p, matchmaking %p, networking messages %p)",
                steam_user.cast_const(),
                friends.cast_const(),
                utils.cast_const(),
                matchmaking.cast_const(),
                messages.cast_const()
            );
            unsafe { release(client, pipe, user) };
            return None;
        }
        // The client tells SDL to pass over every physical controller, as it does in
        // a game Steam started -- where Steam Input stands a virtual one in for them.
        // Nothing does here unless Steam started this, and then it said so before the
        // client loaded; what the client adds on top would leave the title with no
        // controller at all.
        launched.restore();
        Some(Client {
            client,
            steam_user,
            friends,
            utils,
            matchmaking,
            messages,
            get_callback: unsafe { std::mem::transmute::<*mut c_void, GetCallback>(get_callback) },
            free_last_callback: unsafe { std::mem::transmute::<*mut c_void, FreeLastCallback>(free_last_callback) },
            get_call_result: unsafe { std::mem::transmute::<*mut c_void, GetCallResult>(get_call_result) },
            pipe,
            user,
        })
    }

    // A call that returns a CSteamID. The Windows client is built with MSVC, whose
    // member functions return any class through a hidden pointer after `this`,
    // however small; elsewhere it comes back in a register, as the uint64 it holds.
    #[cfg(not(windows))]
    unsafe fn steam_id0(object: *mut c_void, slot: usize) -> u64 {
        unsafe { method::<unsafe extern "C" fn(*mut c_void) -> u64>(object, slot)(object) }
    }
    #[cfg(windows)]
    unsafe fn steam_id0(object: *mut c_void, slot: usize) -> u64 {
        let mut id = 0u64;
        unsafe { method::<unsafe extern "C" fn(*mut c_void, *mut u64) -> *mut u64>(object, slot)(object, &mut id) };
        id
    }
    #[cfg(not(windows))]
    unsafe fn steam_id1<A>(object: *mut c_void, slot: usize, a: A) -> u64 {
        unsafe { method::<unsafe extern "C" fn(*mut c_void, A) -> u64>(object, slot)(object, a) }
    }
    #[cfg(windows)]
    unsafe fn steam_id1<A>(object: *mut c_void, slot: usize, a: A) -> u64 {
        let mut id = 0u64;
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void, *mut u64, A) -> *mut u64>(object, slot)(object, &mut id, a)
        };
        id
    }
    #[cfg(not(windows))]
    unsafe fn steam_id2<A, B>(object: *mut c_void, slot: usize, a: A, b: B) -> u64 {
        unsafe { method::<unsafe extern "C" fn(*mut c_void, A, B) -> u64>(object, slot)(object, a, b) }
    }
    #[cfg(windows)]
    unsafe fn steam_id2<A, B>(object: *mut c_void, slot: usize, a: A, b: B) -> u64 {
        let mut id = 0u64;
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void, *mut u64, A, B) -> *mut u64>(object, slot)(object, &mut id, a, b)
        };
        id
    }

    pub fn steam_id(&self) -> u64 {
        unsafe { Self::steam_id0(self.steam_user, user::GET_STEAM_ID) }
    }

    pub fn logged_on(&self) -> bool {
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void) -> u8>(self.steam_user, user::LOGGED_ON)(self.steam_user) != 0
        }
    }

    pub fn persona_name(&self) -> Vec<u8> {
        let name = unsafe {
            method::<unsafe extern "C" fn(*mut c_void) -> *const c_char>(self.friends, friends::GET_PERSONA_NAME)(
                self.friends,
            )
        };
        text_of(name).unwrap_or_default()
    }

    pub fn set_rich_presence(&self, key: &CStr, value: &CStr) -> bool {
        let set = unsafe {
            method::<unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> u8>(
                self.friends,
                friends::SET_RICH_PRESENCE,
            )
        };
        unsafe { set(self.friends, key.as_ptr(), value.as_ptr()) != 0 }
    }

    pub fn rich_presence(&self, friend: u64, key: &CStr) -> Option<Vec<u8>> {
        let get = unsafe {
            method::<unsafe extern "C" fn(*mut c_void, u64, *const c_char) -> *const c_char>(
                self.friends,
                friends::RICH_PRESENCE,
            )
        };
        text_of(unsafe { get(self.friends, friend, key.as_ptr()) })
    }

    pub fn friend_count(&self, flags: i32) -> i32 {
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void, c_int) -> c_int>(self.friends, friends::COUNT)(
                self.friends,
                flags,
            )
        }
    }

    pub fn friend_by_index(&self, index: i32, flags: i32) -> u64 {
        unsafe { Self::steam_id2(self.friends, friends::BY_INDEX, index, flags) }
    }

    pub fn persona_state(&self, friend: u64) -> i32 {
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void, u64) -> c_int>(self.friends, friends::PERSONA_STATE)(
                self.friends,
                friend,
            )
        }
    }

    pub fn friend_name(&self, friend: u64) -> Option<Vec<u8>> {
        let name = unsafe {
            method::<unsafe extern "C" fn(*mut c_void, u64) -> *const c_char>(self.friends, friends::NAME)(
                self.friends,
                friend,
            )
        };
        text_of(name)
    }

    /// FriendGameInfo_t's game and lobby, when the friend plays something.
    pub fn game_played(&self, friend: u64) -> Option<(u64, u64)> {
        let mut info = [0u64; 3];
        let played = unsafe {
            method::<unsafe extern "C" fn(*mut c_void, u64, *mut u64) -> u8>(self.friends, friends::GAME_PLAYED)(
                self.friends,
                friend,
                info.as_mut_ptr(),
            )
        };
        (played != 0).then_some((info[0], info[2]))
    }

    pub fn medium_avatar(&self, friend: u64) -> i32 {
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void, u64) -> c_int>(self.friends, friends::MEDIUM_AVATAR)(
                self.friends,
                friend,
            )
        }
    }

    pub fn image_size(&self, image: i32) -> Option<(u32, u32)> {
        let (mut width, mut height) = (0u32, 0u32);
        let sized =
            unsafe {
                method::<unsafe extern "C" fn(*mut c_void, c_int, *mut u32, *mut u32) -> u8>(
                    self.utils,
                    utils::IMAGE_SIZE,
                )(self.utils, image, &mut width, &mut height)
            };
        (sized != 0).then_some((width, height))
    }

    pub fn image_rgba(&self, image: i32, rgba: &mut [u8]) -> bool {
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void, c_int, *mut u8, c_int) -> u8>(self.utils, utils::IMAGE_RGBA)(
                self.utils,
                image,
                rgba.as_mut_ptr(),
                rgba.len() as c_int,
            ) != 0
        }
    }

    pub fn create_lobby(&self, kind: i32, members: i32) -> Call {
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void, c_int, c_int) -> Call>(
                self.matchmaking,
                matchmaking::CREATE_LOBBY,
            )(self.matchmaking, kind, members)
        }
    }

    pub fn join_lobby(&self, lobby: u64) -> Call {
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void, u64) -> Call>(self.matchmaking, matchmaking::JOIN_LOBBY)(
                self.matchmaking,
                lobby,
            )
        }
    }

    pub fn leave_lobby(&self, lobby: u64) {
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void, u64)>(self.matchmaking, matchmaking::LEAVE_LOBBY)(
                self.matchmaking,
                lobby,
            )
        }
    }

    pub fn member_count(&self, lobby: u64) -> i32 {
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void, u64) -> c_int>(self.matchmaking, matchmaking::MEMBER_COUNT)(
                self.matchmaking,
                lobby,
            )
        }
    }

    pub fn member(&self, lobby: u64, index: i32) -> u64 {
        unsafe { Self::steam_id2(self.matchmaking, matchmaking::MEMBER, lobby, index) }
    }

    pub fn owner(&self, lobby: u64) -> u64 {
        unsafe { Self::steam_id1(self.matchmaking, matchmaking::OWNER, lobby) }
    }

    pub fn lobby_data(&self, lobby: u64, key: &CStr) -> Option<Vec<u8>> {
        let get = unsafe {
            method::<unsafe extern "C" fn(*mut c_void, u64, *const c_char) -> *const c_char>(
                self.matchmaking,
                matchmaking::GET_LOBBY_DATA,
            )
        };
        text_of(unsafe { get(self.matchmaking, lobby, key.as_ptr()) })
    }

    pub fn set_lobby_data(&self, lobby: u64, key: &CStr, value: &CStr) -> bool {
        let set = unsafe {
            method::<unsafe extern "C" fn(*mut c_void, u64, *const c_char, *const c_char) -> u8>(
                self.matchmaking,
                matchmaking::SET_LOBBY_DATA,
            )
        };
        unsafe { set(self.matchmaking, lobby, key.as_ptr(), value.as_ptr()) != 0 }
    }

    pub fn set_joinable(&self, lobby: u64, joinable: bool) -> bool {
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void, u64, u8) -> u8>(self.matchmaking, matchmaking::SET_JOINABLE)(
                self.matchmaking,
                lobby,
                u8::from(joinable),
            ) != 0
        }
    }

    pub fn set_owner(&self, lobby: u64, owner: u64) -> bool {
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void, u64, u64) -> u8>(self.matchmaking, matchmaking::SET_OWNER)(
                self.matchmaking,
                lobby,
                owner,
            ) != 0
        }
    }

    pub fn invite_to_lobby(&self, lobby: u64, friend: u64) -> bool {
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void, u64, u64) -> u8>(self.matchmaking, matchmaking::INVITE_TO_LOBBY)(
                self.matchmaking,
                lobby,
                friend,
            ) != 0
        }
    }

    pub fn add_string_filter(&self, key: &CStr, value: &CStr, comparison: i32) {
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, c_int)>(
                self.matchmaking,
                matchmaking::ADD_STRING_FILTER,
            )(self.matchmaking, key.as_ptr(), value.as_ptr(), comparison)
        }
    }

    pub fn add_distance_filter(&self, distance: i32) {
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void, c_int)>(self.matchmaking, matchmaking::ADD_DISTANCE_FILTER)(
                self.matchmaking,
                distance,
            )
        }
    }

    pub fn result_count(&self, count: i32) {
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void, c_int)>(self.matchmaking, matchmaking::RESULT_COUNT)(
                self.matchmaking,
                count,
            )
        }
    }

    pub fn request_lobby_list(&self) -> Call {
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void) -> Call>(self.matchmaking, matchmaking::REQUEST_LOBBY_LIST)(
                self.matchmaking,
            )
        }
    }

    pub fn lobby_by_index(&self, index: i32) -> u64 {
        unsafe { Self::steam_id1(self.matchmaking, matchmaking::LOBBY_BY_INDEX, index) }
    }

    /// Unreliable, unordered: the result Steam gives, 1 when it went.
    pub fn send(&self, to: u64, bytes: &[u8], flags: i32, channel: i32) -> i32 {
        let identity = Identity::of(to);
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void, *const Identity, *const u8, u32, c_int, c_int) -> c_int>(
                self.messages,
                messages::SEND,
            )(self.messages, &identity, bytes.as_ptr(), bytes.len() as u32, flags, channel)
        }
    }

    /// The next message on a channel, without waiting: who sent it, and its bytes.
    pub fn receive(&self, channel: i32) -> Option<(Identity, Vec<u8>)> {
        let mut message: *mut u8 = ptr::null_mut();
        let got = unsafe {
            method::<unsafe extern "C" fn(*mut c_void, c_int, *mut *mut u8, c_int) -> c_int>(
                self.messages,
                messages::RECEIVE,
            )(self.messages, channel, &mut message, 1)
        };
        if got < 1 || message.is_null() {
            return None;
        }
        let data: *const u8 = unsafe { ptr::read_unaligned(message.add(MESSAGE_DATA).cast()) };
        let size: i32 = unsafe { ptr::read_unaligned(message.add(MESSAGE_SIZE).cast()) };
        let peer = unsafe { Identity::read(message.add(MESSAGE_PEER)) };
        let bytes = if size > 0 && !data.is_null() {
            unsafe { std::slice::from_raw_parts(data, size as usize) }.to_vec()
        } else {
            Vec::new()
        };
        let release: unsafe extern "C" fn(*mut u8) =
            unsafe { ptr::read_unaligned(message.add(MESSAGE_RELEASE).cast()) };
        unsafe { release(message) };
        Some((peer, bytes))
    }

    pub fn accept(&self, remote: &Identity) -> bool {
        unsafe {
            method::<unsafe extern "C" fn(*mut c_void, *const Identity) -> u8>(self.messages, messages::ACCEPT)(
                self.messages,
                remote,
            ) != 0
        }
    }

    /// The state of the session with a peer, and with `status` what Steam says of
    /// the link.
    pub fn session(&self, peer: u64, status: Option<&mut LinkStatus>) -> i32 {
        let identity = Identity::of(peer);
        let info_query = unsafe {
            method::<unsafe extern "C" fn(*mut c_void, *const Identity, *mut u8, *mut u8) -> c_int>(
                self.messages,
                messages::SESSION_INFO,
            )
        };
        let Some(status) = status else {
            return unsafe { info_query(self.messages, &identity, ptr::null_mut(), ptr::null_mut()) };
        };
        let mut info = [0u64; LINK_INFO / 8];
        let mut real = [0u64; LINK_STATUS / 8];
        let state = unsafe { info_query(self.messages, &identity, info.as_mut_ptr().cast(), real.as_mut_ptr().cast()) };
        let info: &[u8] = unsafe { std::slice::from_raw_parts(info.as_ptr().cast(), LINK_INFO) };
        let real: &[u8] = unsafe { std::slice::from_raw_parts(real.as_ptr().cast(), LINK_STATUS) };
        let debug = &info[184..184 + 127];
        *status = LinkStatus {
            ping: read(real, 4),
            quality_local: read(real, 8),
            quality_remote: read(real, 12),
            out_bytes: read(real, 20),
            in_bytes: read(real, 28),
            send_rate: read(real, 32),
            queue_microseconds: read(real, 48),
            end_reason: read(info, 180),
            end_debug: debug.split(|&c| c == 0).next().unwrap_or_default().to_vec(),
        };
        state
    }

    /// The callbacks waiting on the pipe, each handed to `handle` before it is freed,
    /// the way libsteam_api's manual dispatch reads them.
    pub fn callbacks(&self, mut handle: impl FnMut(Callback)) {
        loop {
            let mut message = [0u64; 3];
            if unsafe { (self.get_callback)(self.pipe, message.as_mut_ptr().cast()) } == 0 {
                return;
            }
            let bytes: &[u8] = unsafe { std::slice::from_raw_parts(message.as_ptr().cast(), 24) };
            let id: i32 = read(bytes, 4);
            let param: *const u8 = read(bytes, 8);
            let at = |offset: usize| unsafe { param.add(offset) };
            let callback = match id {
                CALL_COMPLETED => Callback::CallCompleted(unsafe { ptr::read_unaligned(at(0).cast()) }),
                LOBBY_JOIN_REQUESTED => Callback::LobbyJoinRequested(unsafe { ptr::read_unaligned(at(0).cast()) }),
                PRESENCE_JOIN_REQUESTED => {
                    let connect: [u8; 256] = unsafe { ptr::read_unaligned(at(8).cast()) };
                    Callback::PresenceJoinRequested(connect.split(|&c| c == 0).next().unwrap_or_default().to_vec())
                }
                LOBBY_DATA_UPDATE => Callback::LobbyDataUpdate {
                    lobby: unsafe { ptr::read_unaligned(at(0).cast()) },
                    member: unsafe { ptr::read_unaligned(at(8).cast()) },
                },
                SESSION_REQUEST => Callback::SessionRequest(unsafe { Identity::read(at(0)) }),
                _ => Callback::Other,
            };
            handle(callback);
            unsafe { (self.free_last_callback)(self.pipe) };
        }
    }

    fn call_result(&self, call: Call, id: i32, bytes: &mut [u8]) -> bool {
        let mut failed = 0u8;
        let got = unsafe {
            (self.get_call_result)(self.pipe, call, bytes.as_mut_ptr().cast(), bytes.len() as c_int, id, &mut failed)
        };
        got != 0 && failed == 0
    }

    /// A lobby Steam made: whether the call answered, and what it said.
    pub fn lobby_created(&self, call: Call) -> (bool, LobbyCreated) {
        #[cfg(windows)]
        let mut bytes = [0u8; 16];
        #[cfg(not(windows))]
        let mut bytes = [0u8; 12];
        let answered = self.call_result(call, LOBBY_CREATED, &mut bytes);
        (answered, LobbyCreated { result: read(&bytes, 0), lobby: read(&bytes, LOBBY_CREATED_LOBBY) })
    }

    /// What entering a lobby answered: whether the call did, the lobby and Steam's
    /// response.
    pub fn lobby_entered(&self, call: Call) -> (bool, LobbyEnter) {
        #[cfg(windows)]
        let mut bytes = [0u8; 24];
        #[cfg(not(windows))]
        let mut bytes = [0u8; 20];
        let answered = self.call_result(call, LOBBY_ENTER, &mut bytes);
        (answered, LobbyEnter { lobby: read(&bytes, 0), response: read(&bytes, 16) })
    }

    /// What a lobby list answered: whether the call did, and how many it found.
    pub fn lobby_list(&self, call: Call) -> (bool, u32) {
        let mut bytes = [0u8; 4];
        let answered = self.call_result(call, LOBBY_MATCH_LIST, &mut bytes);
        (answered, read(&bytes, 0))
    }
}

/// Gives the client back the user and the pipe it gave, as a failed or ended
/// connection does.
unsafe fn release(client: *mut c_void, pipe: HSteamPipe, user: HSteamUser) {
    if pipe == 0 {
        return;
    }
    let release_user = unsafe {
        method::<unsafe extern "C" fn(*mut c_void, HSteamPipe, HSteamUser)>(client, steam_client::RELEASE_USER)
    };
    let release_pipe = unsafe {
        method::<unsafe extern "C" fn(*mut c_void, HSteamPipe) -> u8>(client, steam_client::RELEASE_STEAM_PIPE)
    };
    unsafe {
        if user != 0 {
            release_user(client, pipe, user);
        }
        release_pipe(client, pipe);
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        unsafe { release(self.client, self.pipe, self.user) };
    }
}

#[cfg(windows)]
pub fn enter_namespace() -> Option<i32> {
    None
}

#[cfg(not(windows))]
mod namespace {
    use super::*;
    use crate::log::log_warn;
    use std::sync::atomic::{AtomicI32, Ordering::Relaxed};

    const CLONE_NEWUSER: c_int = 0x1000_0000;
    const CLONE_NEWPID: c_int = 0x2000_0000;
    const SYS_PIDFD_OPEN: i64 = 434;
    const O_RDONLY: c_int = 0;
    const O_CLOEXEC: c_int = 0o2_000_000;
    const PR_SET_PDEATHSIG: c_int = 1;
    const SIGINT: c_int = 2;
    const SIGHUP: c_int = 1;
    const SIGKILL: c_int = 9;
    const SIGTERM: c_int = 15;
    const SIG_IGN: usize = 1;
    const EINTR: c_int = 4;

    unsafe extern "C" {
        fn syscall(number: i64, ...) -> i64;
        fn setns(fd: c_int, kind: c_int) -> c_int;
        fn open(path: *const c_char, flags: c_int, ...) -> c_int;
        fn close(fd: c_int) -> c_int;
        fn fork() -> c_int;
        fn prctl(option: c_int, ...) -> c_int;
        fn signal(number: c_int, handler: usize) -> usize;
        fn kill(pid: c_int, signal: c_int) -> c_int;
        fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
        fn fflush(stream: *mut c_void) -> c_int;
        fn strerror(number: c_int) -> *const c_char;
        fn __errno_location() -> *mut c_int;
    }

    static GAME: AtomicI32 = AtomicI32::new(0);

    fn errno() -> c_int {
        unsafe { *__errno_location() }
    }

    fn error_text() -> &'static CStr {
        unsafe { CStr::from_ptr(strerror(errno())) }
    }

    // The parent hands a request to end on to the game. An interrupt from the
    // terminal reaches the game itself, as it reaches every process of the terminal's
    // job.
    extern "C" fn forward(signal: c_int) {
        let game = GAME.load(Relaxed);
        if game > 0 {
            unsafe { kill(game, signal) };
        }
    }

    // The Flatpak's namespaces are owned by a user namespace the player's own uid
    // made, so joining needs no privilege. Both at once through a pidfd (Linux 5.8),
    // which some kernels require; else the user namespace, which grants the right to
    // join the process one, then that.
    fn join(pid: i32) -> bool {
        let pidfd = unsafe { syscall(SYS_PIDFD_OPEN, pid as i64, 0i64) } as c_int;
        if pidfd >= 0 {
            let joined = unsafe { setns(pidfd, CLONE_NEWUSER | CLONE_NEWPID) } == 0;
            unsafe { close(pidfd) };
            if joined {
                return true;
            }
        }
        let at = format!("/proc/{pid}/ns/");
        let user = unsafe { open(c_string(format!("{at}user")).as_ptr(), O_RDONLY | O_CLOEXEC) };
        let process = unsafe { open(c_string(format!("{at}pid")).as_ptr(), O_RDONLY | O_CLOEXEC) };
        let joined = user >= 0
            && process >= 0
            && unsafe { setns(user, CLONE_NEWUSER) } == 0
            && unsafe { setns(process, CLONE_NEWPID) } == 0;
        if user >= 0 {
            unsafe { close(user) };
        }
        if process >= 0 {
            unsafe { close(process) };
        }
        joined
    }

    pub fn enter_namespace() -> Option<i32> {
        let own = std::fs::read_link("/proc/self/ns/pid").ok()?;
        for steam in running_steams() {
            match std::fs::read_link(format!("/proc/{}/ns/pid", steam.pid)) {
                Ok(its) if its != own => {}
                _ => continue,
            }
            let root = path_text(&steam.root);
            if !join(steam.pid) {
                log_warn!(
                    "online: could not join the namespace of Steam %d (%s), which runs from %s: it may drop the game's connection",
                    steam.pid,
                    error_text(),
                    root.as_c_str()
                );
                return None;
            }
            // A process namespace is entered by the children made after joining it.
            unsafe { fflush(ptr::null_mut()) };
            let child = unsafe { fork() };
            if child < 0 {
                log_warn!("online: could not start the game in Steam's namespace (%s)", error_text());
                return None;
            }
            if child == 0 {
                // Ends with the parent, however that ends.
                unsafe { prctl(PR_SET_PDEATHSIG, SIGKILL as std::ffi::c_ulong) };
                log_info!("online: in the namespace of Steam %d, which runs from %s", steam.pid, root.as_c_str());
                return None;
            }
            GAME.store(child, Relaxed);
            unsafe {
                signal(SIGINT, SIG_IGN);
                signal(SIGTERM, forward as *const () as usize);
                signal(SIGHUP, forward as *const () as usize);
            }
            let mut status: c_int = 0;
            while unsafe { waitpid(child, &mut status, 0) } < 0 && errno() == EINTR {}
            let exited = status & 0x7F == 0;
            return Some(if exited { (status >> 8) & 0xFF } else { 128 + (status & 0x7F) });
        }
        None
    }
}

#[cfg(not(windows))]
pub use namespace::enter_namespace;
