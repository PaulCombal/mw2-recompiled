// The launcher's window: its screens -- the main one, the profile, graphics, network,
// maps and bug report screens, and the questions a start asks -- what each entry does,
// and the jobs it runs on a thread of their own (the install, the self-update, a map's
// conversion, the compiled shaders brought up to date). Each frame it describes the
// screen (ui::Frame), which ui.rs draws, and reads the keyboard, a controller and the
// mouse as Dear ImGui read them (input.rs). It is the port of what main.cpp's App was,
// saying what it said, word for word.
#![allow(unsafe_code)]

use crate::boundary::{path_of, utf8};
use crate::draw::{DrawList, Texture, point};
use crate::driver;
use crate::input::{Input, Key};
use crate::maps::{self, Map};
use crate::play::{CAMPAIGN, LAUNCHER, MULTIPLAYER};
use crate::precompile::{self, Pass};
use crate::preview::Preview;
use crate::profile::{self, Player};
use crate::setup::{self, Outcome, Progress, State};
use crate::single;
use crate::sound::{self, Clip};
use crate::ui::{self, Card, Emblem, Frame, Pointer};
use crate::update;
use crate::{play, present, report, settings};
use mw2_launcher::{Check, Release};
use sdl3_sys::everything as sdl;
use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

// The window's icon, a BMP (assets/icons/, tools/icons.py).
const ICON: &[u8] = include_bytes!("../../assets/icons/launcher.bmp");

// Text of any of the kinds the screens are made of, one after the other, as bytes.
macro_rules! cat {
    ($($part:expr),* $(,)?) => {{
        let mut text: Vec<u8> = Vec::new();
        $(text.extend_from_slice(AsRef::<[u8]>::as_ref(&$part));)*
        text
    }};
}

fn number(n: impl std::fmt::Display) -> String {
    n.to_string()
}

/// What an entry does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    None,
    PlayCampaign,
    PlayMultiplayer,
    Install,
    ChooseUpdate,
    Cancel,
    Quit,
    // The profile screen and what it does.
    Profile,
    Back,
    NextPlayer,
    Rename,
    PlayAs,
    Rank,
    Prestige,
    UnlockEverything,
    UnlockMissions,
    AllStars,
    CheckUpdate,
    InstallUpdate,
    // The graphics screen, and the next of what each of its entries sets.
    Graphics,
    Fullscreen,
    AspectRatio,
    Resolution,
    Stretch,
    Antialiasing,
    Anisotropy,
    Shadows,
    FpsLimit,
    Fov,
    // The network screen: which service, and the server's address and password, typed.
    Network,
    NextNetwork,
    ServerAddress,
    ServerPassword,
    // The maps screen; CUSTOM MAPS turned on asks first, on a screen of its own.
    Maps,
    CustomMaps,
    CustomMapsOn,
    CustomMapsCancel,
    PcGame,
    // and its maps: one shown at a time, put in or out of the game, removed after a question.
    AddMap,
    NextMap,
    MapInGame,
    RemoveMap,
    RemoveMapYes,
    RemoveMapNo,
    // The bug report screen: a run of either game with its log kept.
    Report,
    ReportCampaign,
    ReportMultiplayer,
    // The question a start asks when a recorded run was never reported.
    SendPending,
    DiscardPending,
    // The question a start asks when the graphics card cannot compile shaders ahead.
    PlayAnyway,
    DriverBack,
    DriverPage,
}

// More than any screen has, so that a screen's list never moves while it is filled.
const MOST_ENTRIES: usize = 32;

#[derive(Clone)]
struct Entry {
    shown: ui::Entry,
    action: Action,
    heading: Vec<u8>, // the pane, while the entry is the chosen one
    text: Vec<u8>,
    picture: Vec<u8>, // and a picture above its text, a .bmp (UTF-8)
}

impl Entry {
    fn new(action: Action, label: impl Into<Vec<u8>>, heading: impl Into<Vec<u8>>, text: impl Into<Vec<u8>>) -> Entry {
        Entry { shown: ui::Entry::new(label), action, heading: heading.into(), text: text.into(), picture: Vec::new() }
    }
}

// ---- the jobs -------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum JobKind {
    Install,
    CheckUpdate,
    InstallUpdate,
    Compile,
    AddMap,
}

// What a job came to.
struct Ended {
    result: Outcome,
    error: Vec<u8>,
    // JobKind::Compile's.
    compiled: Pass,
    // JobKind::CheckUpdate's: the release as the look left it.
    release: Option<Release>,
}

impl Ended {
    fn of(result: Outcome, error: impl Into<Vec<u8>>) -> Ended {
        Ended { result, error: error.into(), compiled: Pass::default(), release: None }
    }
}

// An install running on its own thread, or a look for a newer version of the launcher or
// its installation, a map converted from the PC game, or the shaders brought up to date.
struct Job {
    kind: JobKind,
    progress: Arc<Progress>,
    compile: Arc<precompile::Progress>,
    // JobKind::Compile: what to do once they are.
    then: Action,
    finished: Arc<AtomicBool>,
    thread: Option<JoinHandle<Ended>>,
}

impl Job {
    fn start(
        kind: JobKind,
        then: Action,
        work: impl FnOnce(&Progress, &precompile::Progress) -> Ended + Send + 'static,
    ) -> Job {
        let (progress, compile) = (Arc::new(Progress::default()), Arc::new(precompile::Progress::default()));
        let finished = Arc::new(AtomicBool::new(false));
        let thread = {
            let (progress, compile, finished) = (progress.clone(), compile.clone(), finished.clone());
            crate::spawn::thread(move || {
                let ended = work(&progress, &compile);
                finished.store(true, Ordering::SeqCst);
                ended
            })
        };
        Job { kind, progress, compile, then, finished, thread: Some(thread) }
    }

    fn cancel(&self) {
        self.progress.cancel.store(true, Ordering::SeqCst);
        self.compile.cancel.store(true, Ordering::SeqCst);
    }

    fn finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    fn join(&mut self) -> Ended {
        // A panic aborts the launcher (panic = "abort"): a thread always returns.
        self.thread
            .take()
            .map(|thread| thread.join().unwrap_or_else(|_| unreachable!()))
            .unwrap_or_else(|| Ended::of(Outcome::Failed, Vec::new()))
    }
}

impl Drop for Job {
    // Waits for a job that was told to stop.
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn install_job(disc: PathBuf, update: PathBuf) -> Job {
    Job::start(JobKind::Install, Action::None, move |progress, _| {
        let (result, error) = setup::install(&disc, &update, progress);
        Ended::of(result, error)
    })
}

fn compile_job(then: Action, device: Vec<u8>) -> Job {
    Job::start(JobKind::Compile, then, move |_, compile| {
        let compiled = precompile::compile(&device, false, compile);
        let result = if compiled.ok {
            Outcome::Done
        } else if compiled.cancelled {
            Outcome::Cancelled
        } else {
            Outcome::Failed
        };
        Ended { result, error: compiled.error.clone(), compiled, release: None }
    })
}

/// `files` added as a map, a map made for the PC game converted first (the PC game's
/// folder looked up before, on the window's thread: the job's thread doesn't touch .env).
fn add_map_job(files: Vec<PathBuf>, cancellable: bool) -> Job {
    let game = maps::pc_game();
    Job::start(JobKind::AddMap, Action::None, move |progress, _| {
        let mut said = Vec::new();
        let result = match maps::add(&files, &mut said, Some(progress), &game) {
            maps::Added::All => Outcome::Done,
            maps::Added::NotAll if cancellable && progress.cancelled() => Outcome::Cancelled,
            _ => Outcome::Failed,
        };
        Ended::of(result, said)
    })
}

pub fn amount(done: u64, total: u64) -> String {
    if total == 0 {
        return String::new();
    }
    format!("{:.1} of {:.1} GB", done as f64 / 1e9, total as f64 / 1e9)
}

// What the terminal mode reads of a job.
pub struct JobView<'a>(&'a Job);

impl JobView<'_> {
    pub fn progress(&self) -> &Progress {
        &self.0.progress
    }
}

/// An install, for the terminal mode, and what it came to.
pub fn run_install(disc: PathBuf, update: PathBuf, watch: impl FnMut(JobView)) -> (Outcome, Vec<u8>) {
    run_job(install_job(disc, update), watch)
}

/// A map added, for the terminal mode, and what it came to.
pub fn run_add_map(files: Vec<PathBuf>, watch: impl FnMut(JobView)) -> (Outcome, Vec<u8>) {
    run_job(add_map_job(files, false), watch)
}

fn run_job(mut job: Job, mut watch: impl FnMut(JobView)) -> (Outcome, Vec<u8>) {
    while !job.finished() {
        std::thread::sleep(Duration::from_millis(250));
        watch(JobView(&job));
    }
    let ended = job.join();
    (ended.result, ended.error)
}

// ---- the file dialog ----------------------------------------------------------------

// The file dialog answers on a thread of its own choosing.
#[derive(Default)]
struct Picked {
    answered: bool,
    failed: bool,
    for_action: Option<Action>,
    path: Vec<u8>,
    paths: Vec<Vec<u8>>, // every file chosen, for a dialog that takes several
}

type Shared = Arc<Mutex<Picked>>;

fn lock(picked: &Shared) -> std::sync::MutexGuard<'_, Picked> {
    picked.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// SDL's answer: the files chosen, none, or null when no dialog could be shown. A folder
// dialog's answer keeps no list of several.
fn take_answer(data: *mut c_void, list: *const *const c_char, several: bool) {
    // SAFETY: the Arc ask() handed over, once per dialog, and SDL's list of C strings
    // ended by a null, alive across the call.
    let picked = unsafe { Arc::from_raw(data.cast::<Mutex<Picked>>()) };
    let mut chosen: Vec<Vec<u8>> = Vec::new();
    if !list.is_null() {
        let mut at = list;
        unsafe {
            while !(*at).is_null() {
                chosen.push(CStr::from_ptr(*at).to_bytes().to_vec());
                at = at.add(1);
            }
        }
    }
    let mut p = lock(&picked);
    p.failed = list.is_null();
    p.path = chosen.first().cloned().unwrap_or_default();
    p.paths = if several { chosen } else { Vec::new() };
    p.answered = true;
}

extern "C" fn answered_files(data: *mut c_void, list: *const *const c_char, _filter: c_int) {
    take_answer(data, list, true);
}

extern "C" fn answered_folder(data: *mut c_void, list: *const *const c_char, _filter: c_int) {
    take_answer(data, list, false);
}

// What a setting with a few values, set with a slider, is: its steps in the slider's
// order, the one kept (the first when it is none of them), and how a step is kept.
struct Steps {
    values: Vec<i32>,
    kept: usize,
    set: fn(i32),
}

fn steps_of(action: Action) -> Option<Steps> {
    let (values, now, set): (Vec<i32>, i32, fn(i32)) = match action {
        Action::Antialiasing => (vec![0, 2, 4, 8], settings::msaa(), settings::set_msaa),
        Action::Anisotropy => (vec![1, 2, 4, 8, 16], settings::anisotropy(), settings::set_anisotropy),
        Action::Shadows => (vec![1, 2, 3, 4], settings::shadow_scale(), settings::set_shadow_scale),
        Action::Resolution => {
            // The rows of the shape kept, smallest first.
            let rows = settings::resolutions(settings::aspect_ratio()).len() as i32;
            ((0..rows).collect(), settings::resolution_row() as i32, |row| {
                settings::set_resolution(settings::aspect_ratio(), row)
            })
        }
        _ => return None,
    };
    let kept = values.iter().position(|&value| value == now).unwrap_or(0);
    Some(Steps { values, kept, set })
}

// A row of RESOLUTION by name, "1920x1200".
fn resolution_name(row: &settings::Resolution) -> String {
    format!("{}x{}", row.width * row.scale as u32, row.height * row.scale as u32)
}

fn ratio_name(ratio: settings::Ratio) -> &'static str {
    match ratio {
        settings::Ratio::Wider => "16:10",
        settings::Ratio::Standard => "4:3",
        settings::Ratio::Wide => "16:9",
    }
}

fn aspect(ratio: settings::Ratio) -> f32 {
    match ratio {
        settings::Ratio::Wider => 16.0 / 10.0,
        settings::Ratio::Standard => 4.0 / 3.0,
        settings::Ratio::Wide => 16.0 / 9.0,
    }
}

fn network_name(network: settings::Network) -> &'static str {
    match network {
        settings::Network::None => "NONE",
        settings::Network::Lan => "LAN",
        settings::Network::Server => "SERVER",
        settings::Network::Steam => "STEAM",
    }
}

const NETWORKS: [settings::Network; 4] =
    [settings::Network::None, settings::Network::Lan, settings::Network::Server, settings::Network::Steam];

// A network's card: what it is, and who it reaches.
fn network_card(network: settings::Network, label: &[u8], chosen: Option<bool>) -> Card {
    let address = settings::server();
    let (emblem, name, title): (Emblem, &[u8], &[u8]) = match network {
        settings::Network::Lan => (Emblem::Lan, b"LAN", b"LOCAL NETWORK"),
        settings::Network::Server => {
            (Emblem::Server, b"SERVER", if address.is_empty() { b"NO ADDRESS SET" } else { &address })
        }
        settings::Network::Steam => (Emblem::Steam, b"STEAM", b"STEAM FRIENDS"),
        settings::Network::None => (Emblem::Computer, b"NO NETWORK", b"THIS COMPUTER ONLY"),
    };
    Card { emblem, name: name.to_vec(), title: title.to_vec(), label: label.to_vec(), chosen }
}

// What the multiplayer can do on a network (src/runtime/online/, docs/multiplayer.md).
// The title's split screen is offline only, and its system link takes one player a copy.
fn network_features(network: settings::Network) -> &'static [&'static str] {
    const SPLIT: &str = "Split screen, up to 4 on this computer";
    const PRIVATE: &str = "Xbox LIVE parties and private matches";
    const FIND: &str = "FIND GAME: Xbox LIVE matchmaking";
    match network {
        settings::Network::None => &[SPLIT, "SYSTEM LINK between copies on this computer"],
        settings::Network::Lan => {
            &[SPLIT, "SYSTEM LINK on the local network", PRIVATE, "Invitations to everyone on the network"]
        }
        settings::Network::Server => {
            &[SPLIT, "SYSTEM LINK with everyone on the server", FIND, PRIVATE, "Invitations to everyone on the server"]
        }
        settings::Network::Steam => {
            &[SPLIT, "SYSTEM LINK with every Steam player", FIND, PRIVATE, "Invitations to your Steam friends"]
        }
    }
}

// As C's toupper in the C locale: the ASCII letters.
fn upper(text: &[u8]) -> Vec<u8> {
    text.to_ascii_uppercase()
}

// ---- the pictures the pane shows -------------------------------------------------

// Each loaded once: a file made again (a map added again) has another time and is loaded
// again.
struct Pictures {
    loaded: BTreeMap<Vec<u8>, (Option<SystemTime>, *mut sdl::SDL_Texture)>,
}

impl Pictures {
    fn get(&mut self, renderer: *mut sdl::SDL_Renderer, path: &[u8]) -> Option<Texture> {
        if path.is_empty() {
            return None;
        }
        let time = std::fs::metadata(path_of(path)).and_then(|m| m.modified()).ok()?;
        let entry = self.loaded.entry(path.to_vec()).or_insert((None, std::ptr::null_mut()));
        if !entry.1.is_null() && entry.0 == Some(time) {
            return Some(present::texture_of(entry.1));
        }
        // SAFETY: the renderer, alive; a texture of this cache's, or null; a C string.
        unsafe {
            if !entry.1.is_null() {
                sdl::SDL_DestroyTexture(entry.1);
            }
            *entry = (Some(time), std::ptr::null_mut());
            let name = CString::new(path).ok()?;
            let surface = sdl::SDL_LoadBMP(name.as_ptr());
            if !surface.is_null() {
                entry.1 = sdl::SDL_CreateTextureFromSurface(renderer, surface);
                sdl::SDL_DestroySurface(surface);
            }
        }
        (!entry.1.is_null()).then(|| present::texture_of(entry.1))
    }
}

impl Drop for Pictures {
    fn drop(&mut self) {
        for &(_, texture) in self.loaded.values() {
            if !texture.is_null() {
                // SAFETY: a texture this cache made.
                unsafe { sdl::SDL_DestroyTexture(texture) };
            }
        }
    }
}

// What the keyboard types: a profile's new name, or the server's address or password.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Server,
    Password,
}

// RANK and PRESTIGE as moved on the screen, by the player's stats file; 0 and -1 are not
// moved. They are written as the screen is left -- BACK, or the window closed -- and only
// where they differ from the file.
#[derive(Clone, Copy)]
struct Moved {
    level: i32,
    prestige: i32,
}

impl Default for Moved {
    fn default() -> Moved {
        Moved { level: 0, prestige: -1 }
    }
}

// The player's stats files.
fn files(player: &Player) -> Vec<PathBuf> {
    let mut files = vec![player.file.clone()];
    files.extend(player.also.iter().map(|other| other.file.clone()));
    files
}

fn is_file(name: &str) -> bool {
    Path::new(name).is_file()
}

struct App {
    window: *mut sdl::SDL_Window,
    state: State,
    job: Option<Job>,
    picked: Shared,
    filters: Box<[sdl::SDL_DialogFileFilter; 2]>,
    folder_start: CString,
    disc: PathBuf,           // kept across a failed download, for the second attempt
    needs_update_file: bool, // the download failed: Install asks for the file
    message: Vec<u8>,        // what the last job or action came to
    message_is_error: bool,
    message_focus: i32, // the profile screen's entry it belongs to
    profile_screen: bool,
    players: Vec<Player>,
    player: usize,
    typing: bool,
    field: Field,
    typed: Vec<u8>,
    campaign: Option<(i32, i32)>, // how far the campaign is unlocked, the Special Ops stars
    moved: BTreeMap<PathBuf, Moved>,
    release: Release, // the newer version a look found, if `newer`
    newer: bool,
    updated: String,    // the version this start is the first of
    game_running: bool, // looked up once a second while the profile screen shows
    report_screen: bool,
    graphics_screen: bool,
    network_screen: bool,
    maps_screen: bool,
    maps_warning: bool, // CUSTOM MAPS is to be turned on: what that lets in, first
    map_list: Vec<Map>, // read when the screen opens and after each change
    map_index: usize,   // the one shown
    map_removing: bool, // REMOVE asks first
    // What FPS LIMIT goes through: the console's 60, two rates screens commonly have, and
    // none. A screen at another rate has its own in place of the common one nearest to
    // it, and until the player chooses, the limit is the screen's.
    fps_limits: [i32; 4],
    screen_fps: i32,
    recording: bool,           // the reported run goes on
    pending_report: bool,      // a recorded run was never reported: the start asks about it
    driver_warned: Action,     // shaders cannot be compiled ahead: what to start past the question
    driver_device: Vec<u8>,    // and the shaders' device it starts with
    driver_card: driver::Card, // that device
    recording_since: Instant,
    game_checked: Instant,
    focus: i32,
    focus_placed: bool,
    quit: bool,
}

impl App {
    fn new(window: *mut sdl::SDL_Window) -> App {
        let now = Instant::now();
        App {
            window,
            state: State::NotInstalled,
            job: None,
            picked: Arc::new(Mutex::new(Picked::default())),
            filters: Box::new([sdl::SDL_DialogFileFilter { name: std::ptr::null(), pattern: std::ptr::null() }; 2]),
            folder_start: CString::default(),
            disc: PathBuf::new(),
            needs_update_file: false,
            message: Vec::new(),
            message_is_error: false,
            message_focus: -1,
            profile_screen: false,
            players: Vec::new(),
            player: 0,
            typing: false,
            field: Field::Name,
            typed: Vec::new(),
            campaign: None,
            moved: BTreeMap::new(),
            release: Release::default(),
            newer: false,
            updated: String::new(),
            game_running: false,
            report_screen: false,
            graphics_screen: false,
            network_screen: false,
            maps_screen: false,
            maps_warning: false,
            map_list: Vec::new(),
            map_index: 0,
            map_removing: false,
            fps_limits: [settings::CONSOLE_FPS, 120, 144, 0],
            screen_fps: settings::CONSOLE_FPS,
            recording: false,
            pending_report: false,
            driver_warned: Action::None,
            driver_device: Vec::new(),
            driver_card: driver::Card::default(),
            recording_since: now,
            game_checked: now,
            focus: 0,
            focus_placed: false,
            quit: false,
        }
    }

    fn read_maps(&mut self) {
        self.map_list = maps::list();
        self.map_index = if self.map_list.is_empty() { 0 } else { self.map_index.min(self.map_list.len() - 1) };
    }

    // Moving or deleting a map's files under a running game would pull them from under it.
    fn maps_busy(&mut self) -> bool {
        if !play::game_running() {
            return false;
        }
        self.message = b"The game is running. Close it to change its maps.".to_vec();
        self.message_is_error = true;
        self.message_focus = self.focus;
        true
    }

    fn find_screen_rate(&mut self) {
        // SAFETY: the window, alive; SDL's own mode, or null.
        unsafe {
            let mode = sdl::SDL_GetCurrentDisplayMode(sdl::SDL_GetDisplayForWindow(self.window));
            if !mode.is_null() {
                self.screen_fps = settings::CONSOLE_FPS.max((*mode).refresh_rate.round() as i64 as i32);
            }
        }
        let limits = &mut self.fps_limits;
        if !limits.contains(&self.screen_fps) {
            let nearer = if (self.screen_fps - limits[1]).abs() <= (self.screen_fps - limits[2]).abs() { 1 } else { 2 };
            limits[nearer] = self.screen_fps;
            limits[1..3].sort();
        }
        if !settings::has_fps_limit() {
            settings::set_fps_limit(self.screen_fps);
        }
    }

    fn ask(&mut self, for_action: Action, what: &'static CStr, pattern: &'static CStr, many: bool) {
        {
            let mut p = lock(&self.picked);
            p.answered = false;
            p.for_action = Some(for_action);
        }
        self.filters[0] = sdl::SDL_DialogFileFilter { name: what.as_ptr(), pattern: pattern.as_ptr() };
        self.filters[1] = sdl::SDL_DialogFileFilter { name: c"All files".as_ptr(), pattern: c"*".as_ptr() };
        let data = Arc::into_raw(self.picked.clone()).cast_mut().cast();
        // SAFETY: the window, alive; the filters live in this App's box until the next
        // dialog, as the C++'s static ones did; `data` is taken back by the answer.
        unsafe {
            sdl::SDL_ShowOpenFileDialog(
                Some(answered_files),
                data,
                self.window,
                self.filters.as_ptr(),
                2,
                std::ptr::null(),
                many,
            )
        };
    }

    // A folder, the same way.
    fn ask_folder(&mut self, for_action: Action, from: &Path) {
        {
            let mut p = lock(&self.picked);
            p.answered = false;
            p.for_action = Some(for_action);
        }
        self.folder_start = CString::new(utf8(from)).unwrap_or_default();
        let start = if self.folder_start.is_empty() { std::ptr::null() } else { self.folder_start.as_ptr() };
        let data = Arc::into_raw(self.picked.clone()).cast_mut().cast();
        // SAFETY: as ask(); the start lives in this App until the next dialog.
        unsafe { sdl::SDL_ShowOpenFolderDialog(Some(answered_folder), data, self.window, start, false) };
    }

    fn install(&mut self, from: PathBuf, update: PathBuf) {
        self.message.clear();
        self.job = Some(install_job(from, update));
        self.focus = 0;
    }

    // Every shader of the game compiled for this computer, if they are not yet
    // (precompile.rs), and then `then`. `device` is the one the game said it draws on, or
    // empty.
    fn compile(&mut self, then: Action, device: Vec<u8>) {
        self.message.clear();
        self.message_is_error = false;
        self.job = Some(compile_job(then, device));
    }

    // A game started, once its shaders are compiled for the device `shaders_for`.
    fn launch(&mut self, action: Action, shaders_for: &[u8]) {
        match action {
            Action::PlayCampaign | Action::PlayMultiplayer => {
                let program = if action == Action::PlayCampaign { CAMPAIGN } else { MULTIPLAYER };
                match play::start(program, shaders_for) {
                    Ok(()) => self.quit = true,
                    Err(error) => {
                        self.message = cat!(program, " could not be started: ", error);
                        self.message_is_error = true;
                    }
                }
            }
            Action::ReportCampaign | Action::ReportMultiplayer => {
                let program = if action == Action::ReportCampaign { CAMPAIGN } else { MULTIPLAYER };
                match report::start(program, shaders_for) {
                    Ok(()) => {
                        self.recording = true;
                        self.recording_since = Instant::now();
                        self.message.clear();
                        self.focus_placed = false;
                    }
                    Err(error) => {
                        self.message = error.into_bytes();
                        self.message_is_error = true;
                    }
                }
                self.message_focus = self.focus;
            }
            _ => {}
        }
    }

    fn act(&mut self, action: Action) {
        // While a job runs, it is all there is to do.
        if self.job.is_some() && action != Action::Cancel && action != Action::Quit {
            return;
        }
        match action {
            Action::PlayCampaign | Action::PlayMultiplayer => self.compile(action, Vec::new()),
            Action::Install => {
                // The disc's files are there already: the update is all that is missing.
                if self.state == State::NeedsUpdate {
                    self.install(PathBuf::new(), PathBuf::new());
                } else {
                    self.ask(Action::Install, c"Disc image", c"iso", false);
                }
            }
            Action::ChooseUpdate => self.ask(Action::ChooseUpdate, c"Title update", c"*", false),
            Action::Cancel => {
                if let Some(job) = &self.job {
                    job.cancel();
                }
            }
            Action::Quit => self.quit = true,
            Action::CheckUpdate => {
                self.message.clear();
                let mut release = self.release.clone();
                self.job = Some(Job::start(JobKind::CheckUpdate, Action::None, move |_, _| {
                    let (result, error) = match update::look_for(&mut release) {
                        Ok(Check::Newer) => (Outcome::Done, String::new()),
                        Ok(Check::UpToDate) => (Outcome::Cancelled, String::new()),
                        Ok(Check::Failed) => (Outcome::Failed, String::new()),
                        Err(error) => (Outcome::Failed, error),
                    };
                    Ended { release: Some(release), ..Ended::of(result, error) }
                }));
            }
            Action::InstallUpdate => {
                // Its programs cannot be replaced under it.
                if play::game_running() {
                    self.message = b"The game is running. Close it to update.".to_vec();
                    self.message_is_error = true;
                    return;
                }
                self.message.clear();
                let release = self.release.clone();
                self.job = Some(Job::start(JobKind::InstallUpdate, Action::None, move |progress, _| {
                    let (result, error) = update::install_release(&release, progress);
                    Ended::of(result, error)
                }));
            }
            Action::ReportCampaign | Action::ReportMultiplayer => {
                if play::game_running() {
                    self.message =
                        b"The game is running. Close it first: the report is of a run started here.".to_vec();
                    self.message_is_error = true;
                    self.message_focus = self.focus;
                } else {
                    self.compile(action, Vec::new());
                }
            }
            Action::Profile | Action::Report | Action::Graphics | Action::Network | Action::Maps | Action::Back => {
                let leaving_profile = self.profile_screen;
                self.game_running = action == Action::Profile && play::game_running();
                self.game_checked = Instant::now();
                self.profile_screen = action == Action::Profile;
                self.report_screen = action == Action::Report;
                self.graphics_screen = action == Action::Graphics;
                self.network_screen = action == Action::Network;
                self.maps_screen = action == Action::Maps;
                self.maps_warning = false;
                self.map_removing = false;
                self.map_index = 0;
                if self.maps_screen {
                    self.read_maps();
                }
                self.message.clear();
                self.message_is_error = false;
                self.focus_placed = false;
                self.player = 0;
                if leaving_profile {
                    self.keep_moved();
                }
                self.read_profile();
            }
            Action::SendPending => {
                self.pending_report = false;
                self.report_screen = true;
                self.finish_report();
            }
            Action::DiscardPending => {
                self.pending_report = false;
                report::discard();
                self.focus_placed = false;
            }
            Action::PlayAnyway | Action::DriverBack => {
                let then = self.driver_warned;
                self.driver_warned = Action::None;
                self.focus_placed = false;
                if action == Action::PlayAnyway {
                    let device = self.driver_device.clone();
                    self.launch(then, &device);
                }
            }
            Action::DriverPage => {
                if let Some(page) = driver::download_page(&self.driver_card) {
                    driver::open(page);
                }
            }
            Action::NextPlayer => self.player = (self.player + 1) % self.players.len(),
            Action::Fullscreen => settings::set_fullscreen(!settings::fullscreen()),
            Action::Stretch => settings::set_stretch(!settings::stretch()),
            Action::CustomMaps => {
                // Off at once; on only past the warning.
                if settings::custom_maps() {
                    settings::set_custom_maps(false);
                } else {
                    self.maps_warning = true;
                    self.focus_placed = false;
                }
            }
            Action::CustomMapsOn | Action::CustomMapsCancel => {
                if action == Action::CustomMapsOn {
                    settings::set_custom_maps(true);
                }
                self.maps_warning = false;
                self.focus_placed = false;
            }
            Action::PcGame => {
                self.message.clear();
                let game = maps::pc_game();
                self.ask_folder(Action::PcGame, &game);
            }
            Action::AddMap => {
                if self.maps_busy() {
                    return;
                }
                self.message.clear();
                self.ask(Action::AddMap, c"Map files", c"ff;arena;iwd", true);
            }
            Action::NextMap => {
                if !self.map_list.is_empty() {
                    self.map_index = (self.map_index + 1) % self.map_list.len();
                }
                self.message.clear();
            }
            Action::MapInGame => {
                if self.maps_busy() || self.map_index >= self.map_list.len() {
                    return;
                }
                let map = self.map_list[self.map_index].clone();
                self.message_is_error = !maps::set_in_game(&map, !map.in_game, &mut self.message);
                self.message_focus = self.focus;
                // The list is the game's first: find the map again where it went.
                self.read_maps();
                for (i, m) in self.map_list.iter().enumerate() {
                    if m.name == map.name {
                        self.map_index = i;
                    }
                }
            }
            Action::RemoveMap => {
                if self.maps_busy() {
                    return;
                }
                self.map_removing = true;
                self.focus_placed = false;
            }
            Action::RemoveMapYes | Action::RemoveMapNo => {
                self.map_removing = false;
                self.focus_placed = false;
                if action == Action::RemoveMapYes && self.map_index < self.map_list.len() && !self.maps_busy() {
                    let map = self.map_list[self.map_index].clone();
                    self.message_is_error = !maps::remove(&map, &mut self.message);
                    self.read_maps();
                    self.message_focus = -1;
                }
            }
            Action::AspectRatio => settings::next_aspect_ratio(),
            Action::Resolution | Action::Antialiasing | Action::Anisotropy | Action::Shadows => {
                // Choosing goes one step up the slider, and round to its start.
                if let Some(steps) = steps_of(action) {
                    (steps.set)(steps.values[(steps.kept + 1) % steps.values.len()]);
                }
            }
            Action::FpsLimit => {
                // The choice after the one kept; one written by hand that is none of them
                // goes back to the first.
                let limits = self.fps_limits;
                let kept = limits.iter().position(|&limit| limit == settings::fps_limit());
                let next = match kept {
                    Some(at) if at + 1 < limits.len() => limits[at + 1],
                    _ => limits[0],
                };
                settings::set_fps_limit(next);
            }
            Action::Fov => {
                // Choosing goes one step wider, and round to the console's.
                let fov = settings::fov();
                settings::set_fov(if fov >= settings::WIDEST_FOV { settings::CONSOLE_FOV } else { fov + FOV_STEP });
            }
            Action::Rename => {
                let name = self.players[self.player].name.clone();
                self.start_typing(Field::Name, name);
            }
            Action::NextNetwork => {
                let next = match settings::current_network() {
                    settings::Network::None => settings::Network::Lan,
                    settings::Network::Lan => settings::Network::Server,
                    settings::Network::Server => settings::Network::Steam,
                    settings::Network::Steam => settings::Network::None,
                };
                settings::set_network(next);
            }
            Action::ServerAddress => self.start_typing(Field::Server, settings::server()),
            Action::ServerPassword => self.start_typing(Field::Password, settings::server_password()),
            // Left and right move them.
            Action::Rank | Action::Prestige => {}
            Action::PlayAs | Action::UnlockEverything | Action::UnlockMissions | Action::AllStars => self.edit(action),
            Action::None => {}
        }
    }

    fn start_typing(&mut self, what: Field, from: Vec<u8>) {
        self.typing = true;
        self.field = what;
        self.typed = from;
        self.message.clear();
        // SAFETY: the window, alive.
        unsafe { sdl::SDL_StartTextInput(self.window) };
    }

    // What a field takes: a name of letters, digits and spaces; an address or password of
    // anything printable but a space.
    fn takes(&self, c: u8) -> bool {
        match self.field {
            Field::Name => self.typed.len() < 15 && (c.is_ascii_alphanumeric() || c == b' '),
            Field::Server => self.typed.len() < 253 && c > b' ' && c < 0x7F,
            Field::Password => self.typed.len() < 63 && c > b' ' && c < 0x7F,
        }
    }

    // The keyboard while something is typed.
    fn type_key(&mut self, event: &sdl::SDL_Event) {
        // SAFETY: the union's field that its type names; SDL's text, a C string.
        unsafe {
            let kind = sdl::SDL_EventType(event.r#type);
            if kind == sdl::SDL_EVENT_TEXT_INPUT {
                if !event.text.text.is_null() {
                    for &c in CStr::from_ptr(event.text.text).to_bytes() {
                        if self.takes(c) {
                            self.typed.push(c);
                        }
                    }
                }
                return;
            }
            if kind != sdl::SDL_EVENT_KEY_DOWN {
                return;
            }
            let (key, modifiers) = (event.key.key, event.key.r#mod.0);
            if key == sdl::SDLK_BACKSPACE && !self.typed.is_empty() {
                self.typed.pop();
            }
            // Ctrl+V or Shift+Insert pastes, as much of it as the field takes.
            let paste = (key == sdl::SDLK_V && modifiers & sdl::SDL_KMOD_CTRL.0 != 0)
                || (key == sdl::SDLK_INSERT && modifiers & sdl::SDL_KMOD_SHIFT.0 != 0);
            if paste {
                let clipboard = sdl::SDL_GetClipboardText();
                if !clipboard.is_null() {
                    for &c in CStr::from_ptr(clipboard).to_bytes() {
                        if c == b'\n' || c == b'\r' {
                            break;
                        }
                        if self.takes(c) {
                            self.typed.push(c);
                        }
                    }
                    sdl::SDL_free(clipboard.cast());
                }
            }
            let keep = key == sdl::SDLK_RETURN || key == sdl::SDLK_KP_ENTER;
            if !keep && key != sdl::SDLK_ESCAPE {
                return;
            }
            self.typing = false;
            sdl::SDL_StopTextInput(self.window);
            if !keep {
                return;
            }
        }
        match self.field {
            Field::Server => settings::set_server(&self.typed),
            Field::Password => settings::set_server_password(&self.typed),
            Field::Name => {
                let id = self.players[self.player].id;
                let done = profile::rename(id, &self.typed);
                self.message_is_error = done.is_err();
                self.message = match done {
                    Ok(()) => cat!("The profile is now ", self.typed, "."),
                    Err(error) => error.into_bytes(),
                };
                self.message_focus = self.focus;
                self.read_profile();
                for (i, p) in self.players.iter().enumerate() {
                    if p.id == id {
                        self.player = i;
                    }
                }
            }
        }
    }

    fn read_profile(&mut self) {
        self.players = profile::players();
        self.player = self.player.min(if self.players.is_empty() { 0 } else { self.players.len() - 1 });
        self.campaign = profile::read_campaign();
    }

    // The rank and prestige the player's rows show: as moved, or as the file has them.
    fn shown_level(&self, p: &Player) -> i32 {
        match self.moved.get(&p.file) {
            Some(moved) if moved.level != 0 => moved.level,
            _ => p.level,
        }
    }
    fn shown_prestige(&self, p: &Player) -> i32 {
        match self.moved.get(&p.file) {
            Some(moved) if moved.prestige >= 0 => moved.prestige,
            _ => p.prestige,
        }
    }

    // What was moved, written: a profile whose rank and prestige are where they were is
    // left untouched.
    fn keep_moved(&mut self) {
        if self.moved.is_empty() {
            return;
        }
        let keep = std::mem::take(&mut self.moved);
        self.read_profile();
        let mut changes: Vec<(Vec<PathBuf>, Moved)> = Vec::new();
        for (file, to) in &keep {
            for p in &self.players {
                if p.file == *file {
                    let change = Moved {
                        level: if to.level != 0 && to.level != p.level { to.level } else { 0 },
                        prestige: if to.prestige >= 0 && to.prestige != p.prestige { to.prestige } else { -1 },
                    };
                    if change.level != 0 || change.prestige >= 0 {
                        changes.push((files(p), change));
                    }
                }
            }
        }
        if changes.is_empty() {
            return;
        }
        // The game writes the profile it has in memory as it ends, over the change.
        if play::game_running() {
            self.message = b"The game is running, so the rank and prestige were not changed. Close it first.".to_vec();
            self.message_is_error = true;
            self.message_focus = -1;
            return;
        }
        for (files, change) in &changes {
            if let Err(error) = profile::set_rank(files, change.level, change.prestige) {
                self.message = error.into_bytes();
                self.message_is_error = true;
                self.message_focus = -1;
            }
        }
        self.read_profile();
    }

    // One change to the profile, and what to say about it.
    fn edit(&mut self, action: Action) {
        let who = self.players.get(self.player).cloned();
        // The list is the newest first, and a change makes its file the newest: the player
        // is found again by the file.
        let file = who.as_ref().map(|p| p.file.clone()).unwrap_or_default();
        let id = who.as_ref().map_or(0, |p| p.id);
        let (done, said): (Result<(), String>, Vec<u8>) = match action {
            Action::PlayAs => (
                who.as_ref().map_or(Err(String::new()), |p| profile::play_as(p.id)),
                who.as_ref()
                    .map_or(Vec::new(), |p| cat!(p.name, " plays at the first controller from the game's next start.")),
            ),
            Action::UnlockEverything => (
                who.as_ref().map_or(Err(String::new()), |p| profile::unlock_everything(&files(p))),
                b"Every challenge is done, and every title, emblem and killstreak unlocked.".to_vec(),
            ),
            Action::UnlockMissions => (profile::unlock_missions(), b"Every campaign mission is unlocked.".to_vec()),
            Action::AllStars => (profile::all_stars(), b"Every Special Ops mission has its three stars.".to_vec()),
            _ => (Err(String::new()), Vec::new()),
        };
        self.message_is_error = done.is_err();
        self.message = match done {
            Ok(()) => said,
            Err(error) => error.into_bytes(),
        };
        self.message_focus = self.focus;
        self.read_profile();
        for (i, p) in self.players.iter().enumerate() {
            if if id != 0 { p.id == id } else { p.file == file } {
                self.player = i;
            }
        }
    }

    fn report_entries(&self) -> Vec<Entry> {
        let mut entries: Vec<Entry> = Vec::with_capacity(MOST_ENTRIES);
        if self.recording {
            entries.push(Entry::new(
                Action::None,
                "RECORDING",
                "THE GAME IS RUNNING",
                "Play until the problem shows, then quit the game.\n\nThe report is made when the game has closed. \
                 If the game closes by itself, that is recorded too.",
            ));
            return entries;
        }
        let how = "The game starts and keeps a log of the run. Play until the problem shows, then quit the game.\n\n\
                   The launcher then writes one report file and opens the project's new-issue page on GitHub with your \
                   system's description filled in. You describe what happened, drag the file in, and submit. \
                   That needs a GitHub account.\n\n\
                   The report is public. Your name, network addresses and folder names are taken out of it.";
        let installed = self.state == State::Installed;
        let mut campaign = Entry::new(Action::ReportCampaign, "CAMPAIGN", "REPORT A BUG IN THE CAMPAIGN", how);
        campaign.shown.enabled = installed && is_file(CAMPAIGN);
        let mut multiplayer =
            Entry::new(Action::ReportMultiplayer, "MULTIPLAYER", "REPORT A BUG IN THE MULTIPLAYER", how);
        multiplayer.shown.enabled = installed && is_file(MULTIPLAYER);
        for entry in [&mut campaign, &mut multiplayer] {
            if !entry.shown.enabled {
                entry.action = Action::None;
                entry.text = b"Install the game first.".to_vec();
            }
        }
        entries.push(campaign);
        entries.push(multiplayer);
        let mut back =
            Entry::new(Action::Back, "BACK", "", "Reports are kept in the reports folder beside the launcher.");
        back.shown.rule_above = true;
        entries.push(back);
        entries
    }

    // The reported run has ended: the file, the page, and what to do with them.
    fn finish_report(&mut self) {
        self.recording = false;
        self.read_profile(); // the run may have made the first profile, which the play entries' cards name
        self.focus_placed = false;
        self.message_focus = -1;
        match report::make() {
            Err(error) => {
                self.message = error.into_bytes();
                self.message_is_error = true;
            }
            Ok(made) => {
                self.message_is_error = false;
                let file = utf8(&made.file);
                self.message = if report::open(&made) {
                    cat!(
                        "The report is\n",
                        file,
                        "\n\nThe issue page has opened in your browser. Describe what happened, \
                         drag that file into the text box, and submit."
                    )
                } else {
                    cat!(
                        "The report is\n",
                        file,
                        "\n\nNo browser could be opened. Attach that file to a new issue at\n\
                         github.com/PaulCombal/mw2-recompiled/issues"
                    )
                };
            }
        }
    }

    fn profile_entries(&self) -> Vec<Entry> {
        let mut entries: Vec<Entry> = Vec::with_capacity(MOST_ENTRIES);
        let add = |entries: &mut Vec<Entry>, action, label: Vec<u8>, enabled: bool, heading: &str, text: Vec<u8>| {
            let mut entry = Entry::new(if enabled { action } else { Action::None }, label, heading, text);
            entry.shown.enabled = enabled;
            entries.push(entry);
            entries.len() - 1
        };
        let running = self.game_running;
        // A profile made on the sign-in screen has no stats to change until its first
        // match has ended.
        let any = !self.players.is_empty() && !self.players[self.player].file.as_os_str().is_empty() && !running;
        let mut who: Vec<u8> = b"No multiplayer profile yet: play the multiplayer once first.".to_vec();
        let (mut level, mut prestige) = (1, 0);
        let (mut named, mut moved_here) = (false, false);
        if !self.players.is_empty() {
            let p = &self.players[self.player];
            level = self.shown_level(p);
            prestige = self.shown_prestige(p);
            moved_here = level != p.level || prestige != p.prestige;
            named = p.id != 0;
            let rank = |offline: bool, level: i32, prestige: i32, played: &str| {
                cat!(
                    if offline { "Offline" } else { "Online" },
                    ": rank ",
                    number(level),
                    ", prestige ",
                    number(prestige),
                    ", last played ",
                    played,
                    "."
                )
            };
            who = if named {
                cat!(p.name, if p.first { ", player 1." } else { "." })
            } else {
                b"Stats of no profile.".to_vec()
            };
            if p.file.as_os_str().is_empty() {
                who.extend_from_slice(b"\nNot played yet.");
            } else {
                who = cat!(who, "\n", rank(p.offline, p.level, p.prestige, &p.played));
            }
            for other in &p.also {
                who = cat!(who, "\n", rank(other.offline, other.level, other.prestige, &other.played));
            }
        }
        // The game keeps the profile in memory and writes it when it ends, over anything
        // changed here meanwhile.
        let close: &str = if running { "\n\nTHE GAME IS RUNNING. Close it to change the profile." } else { "" };
        if self.players.len() > 1 {
            add(
                &mut entries,
                Action::NextPlayer,
                cat!("PLAYER ", number(self.player + 1), " OF ", number(self.players.len())),
                true,
                "PLAYER",
                cat!(who, "\n\nSeveral players have played here. Choose to go to the next one."),
            );
        }
        let rename_text = if self.typing {
            cat!("New name: ", self.typed, "_\n\nENTER keeps it, ESC leaves the name as it was.")
        } else {
            cat!(
                who,
                "\n\n",
                if named {
                    "Type a new name for this profile with the keyboard. On Steam, other players see the Steam name."
                } else {
                    "No profile owns these stats any more: the profile was deleted, or they are from an early version of the game."
                },
                close
            )
        };
        add(&mut entries, Action::Rename, b"RENAME".to_vec(), named && !running, "NAME", rename_text);
        let is_first = named && self.players[self.player].first;
        add(
            &mut entries,
            Action::PlayAs,
            if is_first { b"PLAYER 1".to_vec() } else { b"PLAY AS".to_vec() },
            named && !is_first && !running,
            "PLAYER 1",
            cat!(
                who,
                "\n\n",
                if is_first {
                    "This profile plays at the first controller: the campaign's and the multiplayer's player. Choose another profile here to play as that one."
                } else if named {
                    "Puts this profile at the first controller, from the game's next start."
                } else {
                    "Only a profile can be put at the first controller."
                },
                close
            ),
        );
        // Moved, they are written as the screen is left.
        let kept: &str = if moved_here { "\n\nThe change is kept when you go BACK." } else { "" };
        let max_level = number(profile::MAX_LEVEL);
        add(
            &mut entries,
            Action::Rank,
            cat!("RANK ", number(level)),
            any,
            "MULTIPLAYER RANK",
            cat!(
                who,
                "\n\nLeft and right change it, from 1 to ",
                max_level,
                ", online and offline. A rank unlocks the weapons, perks and equipment of its level and those below; \
                 a lower one locks those above it again. ",
                max_level,
                " unlocks all of them. The attachments and camouflages are the challenges', and stay.",
                kept,
                close
            ),
        );
        add(
            &mut entries,
            Action::Prestige,
            cat!("PRESTIGE ", number(prestige)),
            any,
            "PRESTIGE",
            cat!(
                who,
                "\n\nLeft and right change it, from 0 to ",
                number(profile::MAX_PRESTIGE),
                ". The rank stays.",
                kept,
                close
            ),
        );
        add(
            &mut entries,
            Action::UnlockEverything,
            b"UNLOCK EVERYTHING".to_vec(),
            any,
            "CHALLENGES AND UNLOCKS",
            cat!(
                who,
                "\n\nMarks every challenge as done, which gives every attachment and camouflage, and unlocks every title, emblem and killstreak.",
                close
            ),
        );

        let progress: Vec<u8> = match self.campaign {
            Some((_, stars)) => cat!("Special Ops: ", number(stars), " of 69 stars."),
            None => b"No campaign profile yet: start the campaign once first.".to_vec(),
        };
        let found = self.campaign.is_some();
        let at = add(
            &mut entries,
            Action::UnlockMissions,
            b"UNLOCK ALL MISSIONS".to_vec(),
            found && !running,
            "CAMPAIGN",
            cat!(progress, "\n\nOpens every mission of the campaign in the mission list.", close),
        );
        entries[at].shown.rule_above = true;
        add(
            &mut entries,
            Action::AllStars,
            b"ALL SPEC OPS STARS".to_vec(),
            found && !running,
            "SPECIAL OPS",
            cat!(progress, "\n\nGives every Special Ops mission its three stars, which opens all of them.", close),
        );
        let at = add(
            &mut entries,
            Action::Back,
            b"BACK".to_vec(),
            true,
            "",
            b"A file is copied to <name>.backup, beside it under saves/, before its first change.".to_vec(),
        );
        entries[at].shown.rule_above = true;
        entries
    }

    // The graphics screen.
    fn graphics_entries(&self) -> Vec<Entry> {
        let mut entries: Vec<Entry> = Vec::with_capacity(MOST_ENTRIES);
        const NEXT_START: &str =
            "\n\nIt applies to the campaign and the multiplayer, from the next time either is started.";
        let fullscreen = settings::fullscreen();
        entries.push(Entry::new(
            Action::Fullscreen,
            cat!("FULLSCREEN ", if fullscreen { "ON" } else { "OFF" }),
            "FULLSCREEN",
            cat!(
                "Whether the game starts fullscreen: now ",
                if fullscreen { "it does, at the screen's own resolution." } else { "it starts in a window." },
                "\n\nChoose to switch it. While playing, F8 switches between the two.",
                NEXT_START
            ),
        ));
        let ratio = settings::aspect_ratio();
        entries.push(Entry::new(
            Action::AspectRatio,
            cat!("ASPECT RATIO ", ratio_name(ratio)),
            "ASPECT RATIO",
            cat!(
                "The shape of the game's picture: now ",
                ratio_name(ratio),
                if ratio == settings::Ratio::Wide { ", the console's own." } else { "." },
                "\n\nChoose to go to the next: 16:9, 16:10, 4:3. A screen or window of another shape shows black bars. \
                 The menus and the HUD follow the shape; 4:3 is the console's on a 4:3 TV. On 16:10 and 4:3, the videos \
                 show whole between black bars, and the full-screen pictures fill the screen, their sides cut.",
                NEXT_START
            ),
        ));
        let resolution = settings::current_resolution();
        let sizes = settings::resolutions(ratio).iter().map(resolution_name).collect::<Vec<_>>().join(", ");
        let name = resolution_name(resolution);
        let consoles = resolution.width == 1280 && resolution.height == 720 && resolution.scale == 1;
        entries.push(Entry::new(
            Action::Resolution,
            cat!("RESOLUTION ", name),
            "RESOLUTION",
            cat!(
                "The size the game draws at, whatever the window's: now ",
                name,
                if consoles { ", the console's own." } else { "." },
                " Left and right change it: ",
                sizes,
                ".\n\nA larger one is sharper and needs a faster graphics card. \
                 The best is the screen's own: a smaller one is enlarged to fit it, a larger one made smaller.",
                NEXT_START
            ),
        ));
        let stretch = settings::stretch();
        entries.push(Entry::new(
            Action::Stretch,
            cat!("STRETCH ", if stretch { "ON" } else { "OFF" }),
            "STRETCH",
            cat!(
                "What a screen or window of another shape than the picture shows: now ",
                if stretch {
                    "the picture stretched to fill it."
                } else {
                    "the picture at its own shape, between black bars."
                },
                "\n\nChoose to switch it. Stretching suits a TV that is sent a widescreen picture and shows it at 4:3, \
                 as a CRT TV can: with ASPECT RATIO at 4:3, the picture then fills it at its right shape. \
                 On a screen of the picture's own shape, it changes nothing.",
                NEXT_START
            ),
        ));
        let msaa = settings::msaa();
        entries.push(Entry::new(
            Action::Antialiasing,
            cat!("ANTIALIASING ", if msaa != 0 { format!("{msaa}X") } else { "OFF".into() }),
            "ANTIALIASING",
            cat!(
                "How smooth the edges of what the game draws are: now ",
                if msaa == 2 {
                    "2x, the console's own.".to_string()
                } else if msaa != 0 {
                    format!("{msaa}x, where the console's is 2x.")
                } else {
                    "off, with jagged edges.".to_string()
                },
                " Left and right change it: off, 2x, 4x, 8x.\n\nMore is smoother and needs a faster graphics card; a graphics card \
                 that can't draw as many uses the most it can. Off is the fastest.",
                NEXT_START
            ),
        ));
        let filtering = settings::anisotropy();
        entries.push(Entry::new(
            Action::Anisotropy,
            cat!("ANISOTROPY ", if filtering > 1 { format!("{filtering}X") } else { "CONSOLE".into() }),
            "ANISOTROPIC FILTERING",
            cat!(
                "How sharp the ground, walls and roads stay as they go into the distance: now ",
                if filtering > 1 {
                    format!("{filtering}x.")
                } else {
                    "the console's own, 1x to 4x as each surface asks.".to_string()
                },
                " Left and right change it: console, 2x, 4x, 8x, 16x.\n\nMore is sharper at a slant, and costs little on most \
                 graphics cards. 16x is the PC game's highest.",
                NEXT_START
            ),
        ));
        // RESOLUTION draws the shadow maps at its own scale already, and the game takes
        // the larger of the two.
        let shadows = settings::shadow_scale();
        let drawn = shadows.max(resolution.scale);
        entries.push(Entry::new(
            Action::Shadows,
            cat!("SHADOWS ", if shadows > 1 { format!("{shadows}X") } else { "CONSOLE".into() }),
            "SHADOW RESOLUTION",
            cat!(
                "How sharp the edges of shadows are: now ",
                if drawn > 1 {
                    cat!(
                        "the shadow maps are drawn at ",
                        number(drawn),
                        "x the console's size",
                        if drawn > shadows { cat!(", as RESOLUTION ", name, " draws them.") } else { b".".to_vec() }
                    )
                } else {
                    b"the console's own.".to_vec()
                },
                " Left and right change it: console, 2x, 3x, 4x.\n\nMore makes shadows less jagged and needs a faster \
                 graphics card with more memory. A larger RESOLUTION draws them larger too, and the game takes the larger \
                 of the two. The PC game's are the console's size.",
                NEXT_START
            ),
        ));
        let limit = settings::fps_limit();
        let l = self.fps_limits;
        entries.push(Entry::new(
            Action::FpsLimit,
            cat!("FPS LIMIT ", if limit != 0 { number(limit) } else { "NONE".into() }),
            "FPS LIMIT",
            cat!(
                "How many frames a second the game draws at most: now ",
                if limit == settings::CONSOLE_FPS {
                    "60, the console's own.".to_string()
                } else if limit != 0 {
                    format!("{limit}.")
                } else {
                    "as many as the computer manages.".to_string()
                },
                "\n\nChoose to go to the next: ",
                format!("{}, {}, {}, none. This screen shows {} pictures a second.", l[0], l[1], l[2], self.screen_fps),
                " More than 60 makes the game answer the controller sooner, and looks smoother on a screen that shows more. It needs a faster \
                 computer, and the game was made for 60: if something misbehaves, go back to it.",
                NEXT_START
            ),
        ));
        let fov = settings::fov();
        entries.push(Entry::new(
            Action::Fov,
            cat!("FOV ", number(fov)),
            "FIELD OF VIEW",
            cat!(
                "How wide the game's view is: now ",
                number(fov),
                if fov == settings::CONSOLE_FOV { ", the console's own." } else { ", where the console's is 65." },
                "\n\nWider shows more and needs a faster computer."
            ),
        ));
        let mut back = Entry::new(Action::Back, "BACK", "", "");
        back.shown.rule_above = true;
        entries.push(back);
        entries
    }

    // The network screen.
    fn network_entries(&self) -> Vec<Entry> {
        let mut entries: Vec<Entry> = Vec::with_capacity(MOST_ENTRIES);
        let network = settings::current_network();
        let about: &str = match network {
            settings::Network::None => "The multiplayer plays on this computer only.",
            settings::Network::Lan => "SYSTEM LINK finds the games on your local network.",
            settings::Network::Server => {
                "Everyone on the same server plays together, wherever they are. Whoever runs it gives you its \
                 address and password, below. Anyone on it can join your games: play only with people you trust."
            }
            settings::Network::Steam => {
                "Plays through the Steam you run, as Spacewar; anyone on Steam can join your games. Without \
                 Steam running, it plays as LAN."
            }
        };
        entries.push(Entry::new(
            Action::NextNetwork,
            cat!("NETWORK ", network_name(network)),
            "NETWORK",
            cat!(
                about,
                "\n\nPlayers only meet on the same network mode. Restart the multiplayer for it to \
                 take effect."
            ),
        ));
        let server = network == settings::Network::Server;
        if !server {
            let mut back = Entry::new(Action::Back, "BACK", "", "");
            back.shown.rule_above = true;
            entries.push(back);
            return entries;
        }
        let typing_server = self.typing && self.field == Field::Server;
        let typing_password = self.typing && self.field == Field::Password;
        let address = settings::server();
        entries.push(Entry::new(
            Action::ServerAddress,
            cat!("SERVER ", if address.is_empty() { &b"NONE"[..] } else { &address[..] }),
            "SERVER",
            if typing_server {
                cat!("Address: ", self.typed, "_\n\nCTRL+V pastes. ENTER keeps it, ESC leaves it as it was.")
            } else {
                cat!(
                    "The server's address, such as mw2.example.com or mw2.example.com:3074: now ",
                    if address.is_empty() { b"none.".to_vec() } else { cat!(address, ".") },
                    "\n\nChoose to type it."
                )
            },
        ));
        let password = settings::server_password();
        entries.push(Entry::new(
            Action::ServerPassword,
            cat!("PASSWORD ", if password.is_empty() { "NONE" } else { "SET" }),
            "PASSWORD",
            if typing_password {
                cat!(
                    "Password: ",
                    vec![b'*'; self.typed.len()],
                    "_\n\nCTRL+V pastes. ENTER keeps it, ESC leaves it as it was."
                )
            } else {
                cat!(
                    "The password the server asks for: ",
                    if password.is_empty() { "none now." } else { "set." },
                    "\n\nChoose to type it; an empty one is none."
                )
            },
        ));
        let mut back = Entry::new(Action::Back, "BACK", "", "");
        back.shown.rule_above = true;
        entries.push(back);
        entries
    }

    // The maps screen.
    fn maps_entries(&self) -> Vec<Entry> {
        let mut entries: Vec<Entry> = Vec::with_capacity(MOST_ENTRIES);
        let back = |entries: &mut Vec<Entry>| {
            let mut back = Entry::new(Action::Back, "BACK", "", "");
            back.shown.rule_above = true;
            entries.push(back);
        };
        if self.maps_warning {
            let warning = "The game's own maps are signed, and the multiplayer checks each one before it loads it: a file \
                           somebody changed is refused. A custom map is made by somebody else, and nothing checks it.\n\n\
                           A map is not only pictures and sounds. It holds scripts the game runs, and data the game reads \
                           as it is, trusting that nothing in it is wrong. A map made to do harm, or only a broken one, can \
                           crash the game or change how a match plays. At worst it can take the game over, and with it \
                           reach whatever your account on this computer can: your files and your network.\n\n\
                           Only add maps from people you trust, downloaded from where they published them. The game's own \
                           maps stay checked, and turning custom maps off again makes the game refuse the others.";
            entries.push(Entry::new(Action::CustomMapsCancel, "GO BACK", "BEFORE CUSTOM MAPS ARE ON", warning));
            let mut on = Entry::new(Action::CustomMapsOn, "TURN ON", "BEFORE CUSTOM MAPS ARE ON", warning);
            on.shown.rule_above = true;
            entries.push(on);
            return entries;
        }
        if self.map_removing && self.map_index < self.map_list.len() {
            let map = &self.map_list[self.map_index];
            let text = "The launcher's copy of it is deleted. The files you added it from stay where they are, \
                        and adding them again brings it back.";
            let heading = cat!("REMOVE ", upper(&map.longname), "?");
            let mut keep = Entry::new(Action::RemoveMapNo, "KEEP IT", "", text);
            let mut remove = Entry::new(Action::RemoveMapYes, "REMOVE", "", text);
            for entry in [&mut keep, &mut remove] {
                entry.heading = heading.clone();
                entry.picture = utf8(&map.picture);
            }
            remove.shown.rule_above = true;
            entries.push(keep);
            entries.push(remove);
            return entries;
        }
        let on = settings::custom_maps();
        entries.push(Entry::new(
            Action::CustomMaps,
            cat!("CUSTOM MAPS ", if on { "ON" } else { "OFF" }),
            "CUSTOM MAPS",
            cat!(
                "Whether the multiplayer loads maps other people made, such as maps converted from the PC \
                 game: now ",
                if on { "it does." } else { "it loads only the game's own." },
                "\n\nChoose to switch it",
                if on { "." } else { ": a warning comes first, about what a map from somebody else can do." },
                "\n\nIt applies to the multiplayer from the next time it is started."
            ),
        ));
        {
            let game = maps::pc_game();
            let empty = game.as_os_str().is_empty();
            entries.push(Entry::new(
                Action::PcGame,
                cat!("PC GAME ", if empty { "NONE" } else { "SET" }),
                "PC GAME",
                cat!(
                    "Maps made for the PC game often use pictures and sounds of the PC game's own maps without \
                     carrying them, as every PC player has them. When you have the PC game too (Call of Duty: Modern \
                     Warfare 2 on Steam), a map added here takes them from it; without it, plain ones stand in: \
                     magenta for a picture, silence for a sound.\n\n",
                    if !empty {
                        cat!("The PC game's files used: ", utf8(&game), ".")
                    } else if settings::pc_game().is_some() {
                        b"None is used: MW2_PC_GAME is left empty in the .env file beside the launcher.".to_vec()
                    } else {
                        b"No copy of the PC game was found in Steam's folders.".to_vec()
                    },
                    "\n\nChoose to pick the folder of another copy, the one with main in it."
                ),
            ));
        }
        entries.push(Entry::new(
            Action::AddMap,
            "ADD MAP",
            "ADD MAP",
            cat!(
                "Choose one of the map's files: its others, <map>.ff, <map>_load.ff, <map>.arena and <map>.iwd, are \
                 taken from the same folder. A map made for the PC game (IW4x) is converted for the Xbox 360, which takes a minute or \
                 so; one converted already is added as it is. The .iwd gives its pictures, and its picture here; \
                 what the map takes from the PC game itself comes from PC GAME. The launcher keeps a copy of the map: \
                 the files you chose stay where they are.\n\n\
                 The game has room for ",
                number(maps::IN_GAME),
                " custom maps. A map added while it has them \
                 waits in the maps folder beside the launcher until another is taken out."
            ),
        ));
        if self.map_list.is_empty() {
            back(&mut entries);
            return entries;
        }

        // One map at a time, and what can be done with it.
        let map = &self.map_list[self.map_index.min(self.map_list.len() - 1)];
        let in_game = maps::in_game(&self.map_list);
        let mut about: Vec<u8> = if map.description.is_empty() { Vec::new() } else { cat!(map.description, "\n\n") };
        about.extend_from_slice(if map.in_game {
            if on {
                &b"In the game: in a private match, GAME SETUP, the map, then BONUS MAPS."[..]
            } else {
                b"In the game, once custom maps are on."
            }
        } else {
            b"Not in the game: its files wait in the maps folder beside the launcher."
        });
        about = cat!(about, "\n", number(in_game), " of the game's ", number(maps::IN_GAME), " places are taken.");
        let heading = upper(&map.longname);
        let mut pick = Entry::new(Action::NextMap, heading.clone(), "", about.clone());
        pick.shown.tag = cat!(number(self.map_index + 1), " OF ", number(self.map_list.len()));
        pick.shown.rule_above = true;
        let mut place = Entry::new(
            Action::MapInGame,
            cat!("IN THE GAME ", if map.in_game { "YES" } else { "NO" }),
            "",
            cat!(
                about,
                "\n\nChoose to ",
                if map.in_game {
                    b"take it out of the game. Its files are kept.".to_vec()
                } else if in_game >= maps::IN_GAME {
                    cat!(
                        "put it in the game: first take another out, the game has room for ",
                        number(maps::IN_GAME),
                        "."
                    )
                } else {
                    b"put it in the game.".to_vec()
                }
            ),
        );
        let mut remove = Entry::new(
            Action::RemoveMap,
            "REMOVE",
            "",
            "Deletes the launcher's copy of the map. The files you added it from stay \
             where they are. A question comes first.",
        );
        for entry in [&mut pick, &mut place, &mut remove] {
            entry.heading = heading.clone();
            entry.picture = utf8(&map.picture);
        }
        entries.push(pick);
        entries.push(place);
        entries.push(remove);
        back(&mut entries);
        entries
    }

    // A file the dialog returned, or one dropped on the window.
    fn take(&mut self, for_action: Action, path: &[u8], paths: &[Vec<u8>]) {
        if self.job.is_some() {
            return;
        }
        if for_action == Action::AddMap {
            let files: Vec<PathBuf> = paths.iter().map(|file| path_of(file)).collect();
            if files.is_empty() {
                return;
            }
            self.message_focus = self.focus;
            if maps::converts(&files) {
                // A map made for the PC game is converted first: a minute or so.
                self.message.clear();
                self.job = Some(add_map_job(files, true));
                return;
            }
            let mut said = std::mem::take(&mut self.message);
            self.message_is_error = maps::add(&files, &mut said, None, Path::new("")) != maps::Added::All;
            self.message = said;
            self.read_maps();
            return;
        }
        if path.is_empty() {
            return;
        }
        if for_action == Action::PcGame {
            self.message_focus = self.focus;
            let mut said = std::mem::take(&mut self.message);
            self.message_is_error = !maps::set_pc_game(&path_of(path), &mut said);
            self.message = said;
            return;
        }
        if for_action == Action::ChooseUpdate {
            let disc = if self.state == State::NeedsUpdate { PathBuf::new() } else { self.disc.clone() };
            self.install(disc, path_of(path));
        } else {
            self.disc = path_of(path);
            self.install(self.disc.clone(), PathBuf::new());
        }
    }

    fn poll(&mut self) {
        let mut taken: Option<(Action, Vec<u8>, Vec<Vec<u8>>)> = None;
        {
            let mut p = lock(&self.picked);
            if p.answered {
                p.answered = false;
                let for_action = p.for_action.unwrap_or(Action::None);
                taken = Some((for_action, p.path.clone(), p.paths.clone()));
                if p.failed {
                    // SAFETY: SDL's own string.
                    let error = unsafe { CStr::from_ptr(sdl::SDL_GetError()) }.to_bytes().to_vec();
                    self.message = cat!(
                        "No file dialog could be opened (",
                        error,
                        ").",
                        match for_action {
                            Action::AddMap =>
                                " Copy the map's three files into the game folder beside the launcher instead.",
                            Action::PcGame =>
                                " Write MW2_PC_GAME=<the PC game's folder> in the .env file beside the launcher instead.",
                            _ =>
                                " Drop the file on this window, or install from a terminal:\nmw2-launcher --install path/to/disc.iso",
                        }
                    );
                    self.message_is_error = true;
                    self.message_focus = self.focus;
                }
            }
        }
        if let Some((for_action, path, paths)) = taken
            && for_action != Action::None
        {
            self.take(for_action, &path, &paths);
        }

        // Two seconds in, the game has had time to appear among the processes; the title
        // one starts from its menus is running before it ends itself.
        if self.recording && self.game_checked.elapsed() > Duration::from_secs(1) {
            self.game_checked = Instant::now();
            if self.game_checked.duration_since(self.recording_since) > Duration::from_secs(2) && !play::game_running()
            {
                self.finish_report();
            }
        }

        if self.profile_screen && self.game_checked.elapsed() > Duration::from_secs(1) {
            let was = self.game_running;
            self.game_running = play::game_running();
            self.game_checked = Instant::now();
            // It has ended and written its files: read them again.
            if was && !self.game_running {
                self.read_profile();
            }
        }

        if self.job.as_ref().is_some_and(|job| job.finished() && job.kind == JobKind::Compile) {
            let mut job = self.job.take().unwrap();
            let ended = job.join();
            let result = ended.compiled;
            let compiling = job.compile.checked.load(Ordering::SeqCst);
            let then = job.then;
            // CANCEL pressed while the look was taken: the game is not started even when
            // the look found every shader compiled before it saw the press.
            let stopped = job.compile.cancel.load(Ordering::SeqCst);
            drop(job);
            self.focus_placed = false;
            // A card a newer driver would help is warned of at every start; any other
            // only when the shaders are looked at anew.
            let warn = result.no_libraries && (!result.current || driver::warn_every_start(&result.card));
            if result.ok {
                if warn {
                    self.message = cat!(
                        "The graphics driver for ",
                        result.device,
                        " cannot prepare the game's shaders ahead, so the game may stutter. A newer driver usually can."
                    );
                } else if !result.current {
                    self.message =
                        cat!("Every shader of the game, ", number(result.shaders), ", is compiled for this computer.");
                }
                self.message_is_error = false;
                // Asked first: the start goes on past the question, or not.
                if then == Action::None || stopped {
                } else if warn {
                    self.message.clear();
                    self.driver_warned = then;
                    self.driver_device = result.device_id.clone();
                    self.driver_card = result.card.clone();
                } else {
                    self.launch(then, &result.device_id);
                }
            } else if result.cancelled && !compiling {
                self.message.clear(); // the start was cancelled before any compiling
            } else if result.cancelled {
                self.message = if then == Action::None {
                    b"The shaders were not compiled. The game compiles them before it is played.".to_vec()
                } else {
                    b"The shaders were not compiled, so the game was not started: it is \
                      played only with all of them compiled."
                        .to_vec()
                };
                self.message_is_error = true;
            } else {
                self.message = cat!("The shaders could not be compiled, so the game cannot be played: ", result.error);
                self.message_is_error = true;
            }
        }

        if self.job.as_ref().is_some_and(|job| job.finished() && job.kind == JobKind::AddMap) {
            let ended = self.job.take().unwrap().join();
            self.message = ended.error;
            self.message_is_error = ended.result != Outcome::Done;
            self.read_maps();
            // Back on ADD MAP, which the message is shown on.
            self.focus = self.message_focus.max(0);
            self.focus_placed = true;
        }

        if self.job.as_ref().is_some_and(|job| job.finished() && job.kind != JobKind::Install) {
            let mut job = self.job.take().unwrap();
            let ended = job.join();
            let result = ended.result;
            self.message_is_error = result == Outcome::Failed;
            if job.kind == JobKind::CheckUpdate {
                if let Some(release) = ended.release {
                    self.release = release;
                }
                self.newer = result == Outcome::Done;
                self.message = if self.newer {
                    Vec::new()
                } else if result == Outcome::Failed {
                    ended.error
                } else {
                    cat!("This is the newest version, ", mw2_launcher::current(), ".")
                };
            } else if result == Outcome::Done {
                // The new launcher takes over from here.
                if play::start(LAUNCHER, b"").is_ok() {
                    self.quit = true;
                } else {
                    self.message = b"The new version is installed. Start the launcher again.".to_vec();
                }
                self.newer = false;
            } else {
                self.message = if result == Outcome::Cancelled {
                    b"The update was stopped. Nothing was changed.".to_vec()
                } else {
                    ended.error
                };
            }
        }

        if self.job.as_ref().is_some_and(|job| job.finished()) {
            let ended = self.job.take().unwrap().join();
            let result = ended.result;
            self.needs_update_file = result == Outcome::NoUpdate;
            self.message_is_error = result == Outcome::Failed || result == Outcome::NoUpdate;
            self.message = match result {
                Outcome::Done => b"The game is installed.".to_vec(),
                Outcome::Cancelled => {
                    b"The install was stopped. Starting it again carries on where it left off.".to_vec()
                }
                _ => ended.error,
            };
            if self.needs_update_file {
                self.message = cat!(
                    self.message,
                    "\n\nDownload it yourself from\n",
                    setup::update_address(),
                    "\nthen choose the file with CHOOSE UPDATE FILE."
                );
            }
            self.state = setup::state();
            sound::load();
            self.focus_placed = false;
            // Then every shader of the game, while the player is here.
            if result == Outcome::Done {
                self.compile(Action::None, Vec::new());
            }
        }
    }

    // A look at the compiled shaders that finds them up to date is over in a moment: the
    // screen stays as it was meanwhile, unless it starts a game.
    fn job_shown(&self) -> bool {
        self.job.as_ref().is_some_and(|job| job.kind != JobKind::Compile || job.compile.checked.load(Ordering::SeqCst))
    }
    // A start the player asked for, while that look is taken: said at once, so that the
    // press is seen to have done something.
    fn starting(&self) -> bool {
        self.job.as_ref().is_some_and(|job| {
            job.kind == JobKind::Compile && !job.compile.checked.load(Ordering::SeqCst) && job.then != Action::None
        })
    }
    fn starts_campaign(&self) -> bool {
        let then = self.job.as_ref().map_or(Action::None, |job| job.then);
        then == Action::PlayCampaign || then == Action::ReportCampaign
    }

    fn entries(&self) -> Vec<Entry> {
        if self.pending_report {
            // The launcher was ended with the game, as Steam's "Exit game" does.
            let text = "The game was recorded for a bug report, and the report was not sent. Send it now?\n\n\
                        YES makes the report and opens the page to send it on. NO throws the recording away.";
            return vec![
                Entry::new(Action::SendPending, "YES", "A BUG REPORT WASN'T SENT", text),
                Entry::new(Action::DiscardPending, "NO", "A BUG REPORT WASN'T SENT", text),
            ];
        }
        if self.driver_warned != Action::None {
            let mut text = cat!(
                "The graphics driver for ",
                self.driver_card.name,
                " cannot prepare the game's shaders \
                 before it is played. The game then prepares each one the first time it is needed, \
                 and stutters while it does.\n\n"
            );
            let page = driver::download_page(&self.driver_card);
            let heading = if driver::no_newer_driver(&self.driver_card) {
                text.extend_from_slice(b"No driver for this graphics card can, so the game stutters on it.");
                "YOUR GRAPHICS CARD IS TOO OLD"
            } else {
                text.extend_from_slice(if page.is_some() {
                    b"A newer driver from the maker of your graphics card can. Download it and install it, \
                      then start the game again."
                } else {
                    b"A newer driver from the maker of your graphics card usually can. Install it, then \
                      start the game again."
                });
                "UPDATE YOUR GRAPHICS DRIVER"
            };
            let mut entries = vec![Entry::new(Action::DriverBack, "GO BACK", heading, text.clone())];
            if page.is_some() {
                entries.push(Entry::new(Action::DriverPage, "DOWNLOAD A NEWER DRIVER", heading, text.clone()));
            }
            let mut anyway = Entry::new(Action::PlayAnyway, "PLAY ANYWAY", heading, text);
            anyway.shown.rule_above = true;
            entries.push(anyway);
            return entries;
        }
        let mut entries: Vec<Entry> = Vec::with_capacity(MOST_ENTRIES);
        let add = |entries: &mut Vec<Entry>, action, label: &str, enabled: bool, heading: &str, text: Vec<u8>| {
            let mut entry = Entry::new(if enabled { action } else { Action::None }, label, heading, text);
            entry.shown.enabled = enabled;
            entries.push(entry);
            entries.len() - 1
        };
        if self.starting() {
            let campaign = self.starts_campaign();
            add(
                &mut entries,
                Action::Cancel,
                "CANCEL",
                true,
                if campaign { "CAMPAIGN" } else { "MULTIPLAYER" },
                if campaign { b"Starting the campaign...".to_vec() } else { b"Starting the multiplayer...".to_vec() },
            );
            return entries;
        }
        if self.job_shown() {
            let heading = match self.job.as_ref().map(|job| job.kind) {
                Some(JobKind::Install) => "INSTALLING",
                Some(JobKind::Compile) => "SHADERS",
                Some(JobKind::AddMap) => "ADDING A MAP",
                _ => "UPDATES",
            };
            add(&mut entries, Action::Cancel, "CANCEL", true, heading, Vec::new());
            return entries;
        }
        if self.profile_screen {
            return self.profile_entries();
        }
        if self.report_screen {
            return self.report_entries();
        }
        if self.graphics_screen {
            return self.graphics_entries();
        }
        if self.network_screen {
            return self.network_entries();
        }
        if self.maps_screen {
            return self.maps_entries();
        }
        let installed = self.state == State::Installed;
        let (campaign, multiplayer) = (is_file(CAMPAIGN), is_file(MULTIPLAYER));
        let play_text = |present: bool, program: &str, what: &str| -> Vec<u8> {
            if !installed {
                b"Install the game first.".to_vec()
            } else if !present {
                cat!(program, " is not beside the launcher.")
            } else {
                what.as_bytes().to_vec()
            }
        };
        add(
            &mut entries,
            Action::PlayCampaign,
            "PLAY CAMPAIGN",
            installed && campaign,
            "CAMPAIGN",
            play_text(campaign, CAMPAIGN, "The campaign and Special Ops, alone or in split screen."),
        );
        add(
            &mut entries,
            Action::PlayMultiplayer,
            "PLAY MULTIPLAYER",
            installed && multiplayer,
            "MULTIPLAYER",
            play_text(multiplayer, MULTIPLAYER, "Multiplayer: split screen, system link and private matches."),
        );

        // Until the game is whole, installing it is the next thing to do.
        let install = |entries: &mut Vec<Entry>| -> usize {
            if self.needs_update_file {
                return add(
                    entries,
                    Action::ChooseUpdate,
                    "CHOOSE UPDATE FILE",
                    true,
                    "TITLE UPDATE 6",
                    b"The update could not be downloaded. Choose the file you downloaded yourself.".to_vec(),
                );
            }
            if self.state == State::NeedsUpdate {
                return add(
                    entries,
                    Action::Install,
                    "UPDATE GAME",
                    true,
                    "UPDATE",
                    b"The game is installed from the disc, and this version plays title update 6.\n\n\
                      The update is downloaded (2 MB) and applied to the installed game. Your disc is not needed."
                        .to_vec(),
                );
            }
            add(
                entries,
                Action::Install,
                if installed { "REINSTALL" } else { "INSTALL GAME" },
                true,
                "INSTALL",
                cat!(
                    "Choose the image of your Xbox 360 disc (.iso), or drop it on this window.\n\n\
                     Its files, about 7 GB, are copied into the game folder beside the launcher",
                    if setup::USES_UPDATE { ", and title update 6 is downloaded (2 MB) and applied." } else { "." },
                    "\n\nThe disc is the USA/Europe or the Spanish one, version 1.0.557."
                ),
            )
        };
        let reinstall = installed && !self.needs_update_file;
        if !reinstall {
            let at = install(&mut entries);
            entries[at].shown.rule_above = true;
        }

        // What the game is played with.
        let at = add(
            &mut entries,
            Action::Graphics,
            "GRAPHICS",
            true,
            "GRAPHICS",
            b"Fullscreen or a window, the size the game draws at, its antialiasing, texture filtering and shadows, how many frames a second and how wide its view is.".to_vec(),
        );
        entries[at].shown.rule_above = true;
        add(
            &mut entries,
            Action::Network,
            "NETWORK",
            true,
            "NETWORK",
            cat!(
                "How the multiplayer reaches other players: on this computer only, the local network, a server, or Steam. Now ",
                network_name(settings::current_network()),
                "."
            ),
        );
        add(
            &mut entries,
            Action::Profile,
            "PROFILES",
            true,
            "PROFILES",
            b"Set the multiplayer rank and prestige, unlock everything, and open the campaign's and Special Ops' missions.".to_vec(),
        );
        let at = add(
            &mut entries,
            Action::Maps,
            "MAPS",
            true,
            "MAPS",
            b"Custom maps: maps other people made, such as maps converted from the PC game, in the multiplayer. \
              In beta: some maps don't convert, and some that do don't play yet."
                .to_vec(),
        );
        entries[at].shown.tag = b"BETA".to_vec();

        // The copy itself. A build nobody released has no version to compare.
        let current = mw2_launcher::current();
        let released = !current.is_empty();
        if self.newer {
            let version = &self.release.version;
            let label = format!("UPDATE TO {version}");
            let at = add(
                &mut entries,
                Action::InstallUpdate,
                &label,
                true,
                "UPDATES",
                cat!(
                    "Version ",
                    version,
                    " is out; this is ",
                    current,
                    ".\n\nIt is downloaded (",
                    number((self.release.size + 500000) / 1000000),
                    " MB) and put in place of this one, \
                     and the launcher starts again. The game's files and your saves stay as they are."
                ),
            );
            entries[at].shown.rule_above = true;
        } else {
            let at = add(
                &mut entries,
                Action::CheckUpdate,
                "CHECK FOR UPDATES",
                released,
                "UPDATES",
                if released {
                    cat!("Look for a newer version than this one, ", current, ".")
                } else {
                    b"This build was not made as a release, so there is no version to compare.".to_vec()
                },
            );
            entries[at].shown.rule_above = true;
        }
        if reinstall {
            install(&mut entries);
        }
        add(
            &mut entries,
            Action::Report,
            "REPORT A BUG",
            true,
            "REPORT A BUG",
            b"Something wrong with the game? Send the game's report data to GitHub.\n\nA GitHub account is required."
                .to_vec(),
        );
        let at = add(&mut entries, Action::Quit, "QUIT", true, "", Vec::new());
        entries[at].shown.rule_above = true;
        entries
    }
}

const FOV_STEP: i32 = 5;

const DOWN: [Key; 3] = [Key::DownArrow, Key::GamepadDpadDown, Key::GamepadLStickDown];
const UP: [Key; 3] = [Key::UpArrow, Key::GamepadDpadUp, Key::GamepadLStickUp];
const LEFT: [Key; 3] = [Key::LeftArrow, Key::GamepadDpadLeft, Key::GamepadLStickLeft];
const RIGHT: [Key; 3] = [Key::RightArrow, Key::GamepadDpadRight, Key::GamepadLStickRight];
const CHOOSE: [Key; 4] = [Key::Enter, Key::KeypadEnter, Key::Space, Key::GamepadFaceDown];
const BACK: [Key; 2] = [Key::Escape, Key::GamepadFaceRight];

fn error_text() -> String {
    // SAFETY: SDL's own string.
    unsafe { CStr::from_ptr(sdl::SDL_GetError()) }.to_string_lossy().into_owned()
}

/// The window; `then` is what to do at once, `device` the one the game draws on (--play).
pub fn run(updated: String, then: Action, device: Vec<u8>) -> i32 {
    // SAFETY: SDL's calls, on this, the main thread; every pointer handed over is SDL's
    // own or this function's, alive across the call.
    unsafe {
        if !sdl::SDL_Init(sdl::SDL_INIT_VIDEO | sdl::SDL_INIT_GAMEPAD) {
            eprintln!(
                "No window could be opened ({}). Install from a terminal: mw2-launcher --install <disc>",
                error_text()
            );
            return 1;
        }
        // The design is 1280x720; a smaller desktop gets it scaled down.
        let mut scale = 1.0f32;
        let mut bounds = sdl::SDL_Rect::default();
        if sdl::SDL_GetDisplayUsableBounds(sdl::SDL_GetPrimaryDisplay(), &mut bounds) {
            scale = ui::std_min(1.0, ui::std_min(bounds.w as f32 / 1320.0, bounds.h as f32 / 780.0));
        }
        let (mut window, mut renderer) = (std::ptr::null_mut(), std::ptr::null_mut());
        if !sdl::SDL_CreateWindowAndRenderer(
            c"Modern Warfare 2".as_ptr(),
            crate::draw::int(1280.0 * scale),
            crate::draw::int(720.0 * scale),
            sdl::SDL_WindowFlags::default(),
            &mut window,
            &mut renderer,
        ) {
            sdl::SDL_ShowSimpleMessageBox(
                sdl::SDL_MESSAGEBOX_ERROR,
                c"Modern Warfare 2".as_ptr(),
                sdl::SDL_GetError(),
                std::ptr::null_mut(),
            );
            return 1;
        }
        let mut app = App::new(window);
        let icon = sdl::SDL_LoadBMP_IO(sdl::SDL_IOFromConstMem(ICON.as_ptr().cast(), ICON.len()), true);
        if !icon.is_null() {
            sdl::SDL_SetWindowIcon(window, icon);
            sdl::SDL_DestroySurface(icon);
        }
        sdl::SDL_SetRenderVSync(renderer, 1);
        // In front, with the keyboard's focus: SDL hears no controller for a window that
        // has not got it, and a window the system opened behind another -- it does when
        // the program starting it was not in front -- answers to nothing but the mouse
        // until it is clicked.
        sdl::SDL_RaiseWindow(window);
        app.find_screen_rate();
        app.read_profile(); // the main screen's cards name the first profile

        let mut input = Input::new(window);
        let (atlas, fonts) = ui::load_fonts(scale).expect("the launcher's own fonts");
        let font = present::atlas_texture(renderer, &atlas);
        let backdrop = ui::Backdrop::new(renderer);
        let mut pictures = Pictures { loaded: BTreeMap::new() };
        let mut preview = Preview::new();
        let font_texture = font.unwrap_or(Texture(0));
        let mut list = DrawList::new(&atlas, font_texture, point(0.0, 0.0));
        let mut colours = Vec::new();

        app.state = setup::state();
        sound::load();
        app.updated = updated;
        app.pending_report = report::pending() && !play::game_running();
        if then != Action::None && app.state == State::Installed {
            app.compile(then, device);
        }
        while !app.quit {
            let was_typing = app.typing;
            let mut event: sdl::SDL_Event = std::mem::zeroed();
            while sdl::SDL_PollEvent(&mut event) {
                input.event(&event);
                let kind = sdl::SDL_EventType(event.r#type);
                if kind == sdl::SDL_EVENT_QUIT || kind == sdl::SDL_EVENT_WINDOW_CLOSE_REQUESTED {
                    if let Some(job) = &app.job {
                        job.cancel();
                    }
                    app.quit = true;
                }
                // A profile's new name, or the server's address or password, typed.
                if app.typing {
                    app.type_key(&event);
                }
                // A disc image dropped on the window installs from it; while the update
                // is what is asked for, the file is the update.
                if kind == sdl::SDL_EVENT_DROP_FILE && !event.drop.data.is_null() {
                    let path = CStr::from_ptr(event.drop.data).to_bytes().to_vec();
                    let for_action = if app.needs_update_file { Action::ChooseUpdate } else { Action::Install };
                    app.take(for_action, &path, &[]);
                }
            }
            app.poll();
            // Another start of the launcher asked for this one (single.rs).
            if single::system::woken() {
                // A window minimized on X11 is a hidden one to SDL, which restores only
                // what it shows.
                if sdl::SDL_GetWindowFlags(window).0 & sdl::SDL_WINDOW_MINIMIZED.0 != 0 {
                    sdl::SDL_ShowWindow(window);
                    sdl::SDL_RestoreWindow(window);
                }
                sdl::SDL_RaiseWindow(window);
            }
            // The key that ended the typing is not also a choice.
            let typed = was_typing && !app.typing;

            input.new_frame();
            let (size, pixel_scale) = input.display();
            list.clear(font_texture, size);

            let entries = app.entries();
            let count = entries.len() as i32;
            let placed = app.focus_placed;
            if !app.focus_placed {
                // The first thing a player can do.
                app.focus = entries.iter().position(|entry| entry.shown.enabled).unwrap_or(0) as i32;
                app.focus_placed = true;
            }
            let focus_before = app.focus;
            if input.pressed(&DOWN, true) {
                app.focus = (app.focus + 1) % count;
            }
            if input.pressed(&UP, true) {
                app.focus = (app.focus + count - 1) % count;
            }
            app.focus = app.focus.clamp(0, count - 1);
            let mut chosen = if input.pressed(&CHOOSE, false) { app.focus } else { -1 };
            // While a name is typed the keys are its letters, and the entry stays.
            if app.typing || typed {
                app.focus = focus_before;
                chosen = -1;
            } else if (app.job_shown() || app.starting()) && input.pressed(&BACK, false) {
                if let Some(job) = &app.job {
                    job.cancel();
                }
            } else if app.driver_warned != Action::None && input.pressed(&BACK, false) {
                sound::play(Clip::Click);
                app.act(Action::DriverBack);
                input.end_frame();
                continue;
            } else if (app.profile_screen
                || app.graphics_screen
                || app.network_screen
                || app.maps_screen
                || (app.report_screen && !app.recording))
                && input.pressed(&BACK, false)
            {
                sound::play(Clip::Click);
                app.act(if app.maps_warning {
                    Action::CustomMapsCancel
                } else if app.map_removing {
                    Action::RemoveMapNo
                } else {
                    Action::Back
                });
                input.end_frame();
                continue;
            }

            let mut frame =
                Frame { entries: entries.iter().map(|entry| entry.shown.clone()).collect(), ..Frame::default() };
            frame.focus = app.focus;
            let current = &entries[app.focus as usize];
            frame.heading = current.heading.clone();
            if app.network_screen {
                // Every network, the one the multiplayer plays on marked, and what it allows.
                let network = settings::current_network();
                frame.cards = NETWORKS
                    .iter()
                    .map(|&each| {
                        let chosen = each == network;
                        network_card(each, if chosen { b"ACTIVE" } else { b"" }, Some(chosen))
                    })
                    .collect();
                frame.features = network_features(network).iter().map(|feature| feature.as_bytes().to_vec()).collect();
            }
            if (current.action == Action::PlayCampaign || current.action == Action::PlayMultiplayer)
                && current.shown.enabled
            {
                // Who and on what the game will play, so a player who meant to join
                // friends sees before starting that it is set otherwise.
                let network = settings::current_network();
                frame.cards.push(network_card(network, b"NETWORK SETTING", None));
                // On Steam the others see Steam's name for the player, which the launcher
                // does not know; the stats are still the profile's.
                let first = app.players.iter().find(|p| p.first && !p.name.is_empty());
                let steam = network == settings::Network::Steam;
                let profile = |name: Vec<u8>, title: Vec<u8>| Card {
                    emblem: Emblem::Profile,
                    name,
                    title,
                    label: b"DEFAULT PROFILE".to_vec(),
                    chosen: None,
                };
                frame.cards.push(match first {
                    None => profile(
                        if steam { b"STEAM PROFILE".to_vec() } else { b"NEW PROFILE".to_vec() },
                        if steam { b"PLAYER 1".to_vec() } else { b"PLAYER 1, MADE AT THE FIRST START".to_vec() },
                    ),
                    Some(first) => profile(
                        if steam { b"STEAM PROFILE".to_vec() } else { first.name.clone() },
                        if current.action == Action::PlayMultiplayer && !first.offline {
                            cat!(
                                "PLAYER 1 \u{b7} RANK ",
                                number(first.level),
                                if first.prestige != 0 {
                                    cat!(" \u{b7} PRESTIGE ", number(first.prestige))
                                } else {
                                    Vec::new()
                                }
                            )
                        } else {
                            b"PLAYER 1".to_vec()
                        },
                    ),
                });
            }
            // The message, when it belongs to what is chosen.
            let shown_here = app.message_focus < 0 || app.focus == app.message_focus;
            let shows_message = if app.report_screen {
                !app.message.is_empty() && !app.recording && shown_here
            } else if app.profile_screen || app.maps_screen {
                !app.message.is_empty() && shown_here
            } else {
                !app.message.is_empty()
                    && (matches!(
                        current.action,
                        Action::Install | Action::ChooseUpdate | Action::CheckUpdate | Action::InstallUpdate
                    ) || app.message_is_error)
            };
            if app.job_shown() && app.job.as_ref().is_some_and(|job| job.kind == JobKind::Compile) {
                // The game's files read, then their shaders compiled.
                let progress = &app.job.as_ref().unwrap().compile;
                let shaders = progress.shaders.load(Ordering::SeqCst);
                let compiled = progress.compiled.load(Ordering::SeqCst);
                let files = progress.files.load(Ordering::SeqCst);
                let read = progress.files_read.load(Ordering::SeqCst);
                frame.busy = true;
                frame.steps = 2;
                frame.step = if shaders != 0 { 2 } else { 1 };
                frame.step_title =
                    if shaders != 0 { b"Compiling the shaders".to_vec() } else { b"Reading the game's files".to_vec() };
                frame.amount = if shaders != 0 {
                    cat!(number(compiled), " of ", number(shaders))
                } else {
                    cat!(number(read), " of ", number(files))
                };
                frame.fraction = if shaders != 0 {
                    compiled as f32 / shaders as f32
                } else if files != 0 {
                    read as f32 / files as f32
                } else {
                    -1.0
                };
                frame.text = if progress.cancel.load(Ordering::SeqCst) {
                    b"Stopping...".to_vec()
                } else {
                    let why = progress.why.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
                    cat!(
                        why,
                        "\n\nEvery shader of the game is compiled for this computer before it is played, \
                         so that none is compiled in play, where it would stutter. It is done again \
                         when the game, the graphics driver, the graphics card, the resolution or \
                         the game's files change."
                    )
                };
            } else if app.job_shown() {
                let job = app.job.as_ref().unwrap();
                let progress = job.progress.state();
                frame.busy = progress.steps > 0;
                frame.step = progress.step;
                frame.steps = progress.steps;
                frame.step_title = progress.title.clone();
                frame.detail = progress.detail.clone();
                frame.amount = amount(progress.done, progress.total).into_bytes();
                frame.fraction =
                    if progress.total != 0 { (progress.done as f64 / progress.total as f64) as f32 } else { -1.0 };
                frame.text = if job.progress.cancelled() {
                    b"Stopping...".to_vec()
                } else {
                    match job.kind {
                        JobKind::CheckUpdate => b"Looking for a newer version...".to_vec(),
                        JobKind::InstallUpdate => b"The launcher starts again when it is done.".to_vec(),
                        JobKind::AddMap => {
                            b"The map was made for the PC game: it is converted for the Xbox 360, with what it needs \
                                             from the game's own files. It takes a minute or so."
                                .to_vec()
                        }
                        _ => b"Stopping and starting again later carries on where it left off.".to_vec(),
                    }
                };
            } else if shows_message {
                frame.text = app.message.clone();
                frame.error = app.message_is_error;
            } else {
                frame.text = current.text.clone();
            }
            frame.status = match app.state {
                State::Installed => b"The game is installed.".to_vec(),
                State::NeedsUpdate => b"The game is installed from the disc and needs title update 6.".to_vec(),
                State::NotInstalled => b"The game is not installed.".to_vec(),
            };
            frame.corner =
                if setup::USES_UPDATE { b"TITLE UPDATE 6".to_vec() } else { b"DISC VERSION 1.0.557".to_vec() };
            if !mw2_launcher::current().is_empty() {
                frame.corner = cat!(mw2_launcher::current(), "   ", frame.corner);
            }
            if !app.updated.is_empty() {
                frame.status = cat!("Updated to ", app.updated, ". ", frame.status);
            }
            let screen =
                app.profile_screen || app.report_screen || app.graphics_screen || app.network_screen || app.maps_screen;
            frame.hint = if app.recording {
                Vec::new()
            } else if screen {
                b"ENTER OR (A) TO CHOOSE, ESC OR (B) TO GO BACK".to_vec()
            } else {
                b"ENTER OR (A) TO CHOOSE".to_vec()
            };

            if current.action == Action::NextMap && app.map_list.len() > 1 {
                // Left and right go through the maps, as a choice goes to the next.
                let count = app.map_list.len();
                if input.pressed(&LEFT, true) {
                    app.map_index = (app.map_index + count - 1) % count;
                    app.message.clear();
                    sound::play(Clip::Over);
                }
                if input.pressed(&RIGHT, true) {
                    app.map_index = (app.map_index + 1) % count;
                    app.message.clear();
                    sound::play(Clip::Over);
                }
                frame.hint = b"LEFT AND RIGHT FOR ANOTHER MAP, ESC OR (B) TO GO BACK".to_vec();
            }
            frame.picture = pictures.get(renderer, &current.picture);

            let steps = steps_of(current.action);
            if let Some(steps) = &steps {
                frame.slider = steps.kept as f32 / (steps.values.len() - 1) as f32;
                frame.hint = b"LEFT AND RIGHT TO CHANGE, ESC OR (B) TO GO BACK".to_vec();
            }
            if current.action == Action::Fov {
                let range = settings::WIDEST_FOV - settings::CONSOLE_FOV;
                frame.slider = (settings::fov() - settings::CONSOLE_FOV) as f32 / range as f32;
                frame.fov = settings::fov() as f32;
                frame.aspect = aspect(settings::aspect_ratio());
                frame.hint = b"LEFT AND RIGHT TO CHANGE, ESC OR (B) TO GO BACK".to_vec();
            }
            // RANK and PRESTIGE: the same, over what the player's stats can hold.
            let rank_row = current.action == Action::Rank || current.action == Action::Prestige;
            let rank_low = if current.action == Action::Rank { 1 } else { 0 };
            let rank_high = if current.action == Action::Rank { profile::MAX_LEVEL } else { profile::MAX_PRESTIGE };
            if rank_row {
                let p = &app.players[app.player];
                let value = if current.action == Action::Rank { app.shown_level(p) } else { app.shown_prestige(p) };
                frame.slider = (value - rank_low) as f32 / (rank_high - rank_low) as f32;
                frame.hint = b"LEFT AND RIGHT TO CHANGE, ESC OR (B) TO GO BACK".to_vec();
            }

            let mut slid = -1.0f32;
            let pointer = Pointer {
                pos: input.mouse_pos(),
                delta: input.mouse_delta(),
                clicked: input.clicked(),
                down: input.mouse_down(),
                time: input.time(),
                delta_time: input.delta_time(),
            };
            let pointed = ui::draw(
                &mut list,
                &atlas,
                &fonts,
                backdrop.steam(),
                &frame,
                scale,
                &mut app.focus,
                &mut slid,
                &pointer,
                &mut preview,
            );
            if let Some(steps) = &steps {
                // A step a press to either side, or the one nearest where the pointer holds it.
                let last = steps.values.len() as i32 - 1;
                let mut step = steps.kept as i32;
                if input.pressed(&LEFT, true) {
                    step -= 1;
                }
                if input.pressed(&RIGHT, true) {
                    step += 1;
                }
                if slid >= 0.0 {
                    step = (slid * last as f32).round() as i64 as i32;
                }
                let step = step.clamp(0, last);
                if step != steps.kept as i32 {
                    (steps.set)(steps.values[step as usize]);
                    sound::play(Clip::Over);
                }
            }
            if current.action == Action::Fov {
                // The slider: a step a press to either side, or wherever the pointer holds it.
                let mut fov = settings::fov();
                if input.pressed(&LEFT, true) {
                    fov -= FOV_STEP;
                }
                if input.pressed(&RIGHT, true) {
                    fov += FOV_STEP;
                }
                if slid >= 0.0 {
                    let range = (settings::WIDEST_FOV - settings::CONSOLE_FOV) as f32;
                    fov = settings::CONSOLE_FOV + ((slid * range / FOV_STEP as f32).round() as i64 as i32) * FOV_STEP;
                }
                let fov = fov.clamp(settings::CONSOLE_FOV, settings::WIDEST_FOV);
                if fov != settings::fov() {
                    settings::set_fov(fov);
                    sound::play(Clip::Over);
                }
            }
            if rank_row {
                let p = app.players[app.player].clone();
                let rank = current.action == Action::Rank;
                let was = if rank { app.shown_level(&p) } else { app.shown_prestige(&p) };
                let mut value = was;
                if input.pressed(&LEFT, true) {
                    value -= 1;
                }
                if input.pressed(&RIGHT, true) {
                    value += 1;
                }
                if slid >= 0.0 {
                    value = rank_low + (slid * (rank_high - rank_low) as f32).round() as i64 as i32;
                }
                let value = value.clamp(rank_low, rank_high);
                if value != was {
                    let to = app.moved.entry(p.file.clone()).or_default();
                    if rank {
                        to.level = value;
                    } else {
                        to.prestige = value;
                    }
                    app.message.clear();
                    sound::play(Clip::Over);
                }
            }
            if pointed >= 0 {
                chosen = pointed;
            }
            // The game's menus tick as another entry is reached and sound a choice; a
            // screen that has just come up has reached nothing.
            if app.focus != focus_before && placed {
                sound::play(Clip::Over);
            }
            if chosen >= 0 && entries[chosen as usize].shown.enabled {
                sound::play(Clip::Click);
            }
            // Reading a message dismisses it: the next move shows the entries' own text
            // again.
            if chosen >= 0 && !app.message_is_error {
                app.message.clear();
            }
            if chosen >= 0 && chosen != app.focus {
                app.focus = chosen;
            }
            if chosen >= 0 {
                app.act(entries[chosen as usize].action);
            }

            input.end_frame();
            sdl::SDL_SetRenderDrawColor(renderer, 40, 40, 38, 255);
            sdl::SDL_RenderClear(renderer);
            backdrop.draw(renderer, sdl::SDL_GetTicks() as f64 / 1000.0);
            present::present(renderer, &list, size, pixel_scale, &mut colours);
            sdl::SDL_RenderPresent(renderer);
        }
        app.keep_moved(); // the window closed on the profile screen
        app.job = None; // waits for a job that was told to stop

        drop(input);
        if let Some(font) = font {
            sdl::SDL_DestroyTexture(present::sdl_texture(font));
        }
        drop(backdrop);
        drop(pictures);
        sdl::SDL_DestroyRenderer(renderer);
        sdl::SDL_DestroyWindow(window);
        sdl::SDL_Quit();
    }
    0
}
