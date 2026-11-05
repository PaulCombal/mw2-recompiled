// Reading where the player stands, and walking them somewhere: see runtime/player.h.
//
// The address chain is the one the title's own `viewpos` command walks (sub_821237C0 in
// the multiplayer), which is the only place in the image that turns "the local player"
// into an address without going through the renderer. title.h holds the four numbers;
// everything here is arithmetic on them.
//
// The time is the caller's, as for report.rs. Nothing here calls the guest.

use crate::env;
use crate::log::{c_string, format_text, log_error, log_info, log_warn};
use crate::runtime::clock;
use crate::runtime::console;
use crate::runtime::guest_memory;
use crate::runtime::platform;
use crate::runtime::scan;
use crate::title::{T_CLIENT_ANGLES, T_CLIENT_ORIGIN, T_CLIENT_VALID, T_DATA_ClientStates};
use std::ffi::CStr;
use std::fs::{self, File};
use std::io::{Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::Duration;

const MILLISECOND: u64 = 1_000_000;
const SECOND: u64 = 1_000_000_000;

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct Where {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub yaw: f32,
    pub pitch: f32,
}

#[derive(Copy, Clone, PartialEq)]
#[repr(i32)]
pub enum Press {
    None,
    A,
    Up,
    Left,
    Fire,
}

// A place on a route, and what was done there: nothing, d-pad left, or the right
// trigger, aimed at `yaw` and `pitch`.
#[derive(Copy, Clone, PartialEq)]
enum Act {
    Walk,
    Left,
    Fire,
}

#[derive(Copy, Clone)]
struct Point {
    x: f32,
    y: f32,
    act: Act,
    yaw: f32,
    pitch: f32,
    view: bool, // a walked point that says where the player looked
}

impl Point {
    const fn at(x: f32, y: f32) -> Self {
        Self { x, y, act: Act::Walk, yaw: 0.0, pitch: 0.0, view: false }
    }
}

type Route = Vec<Point>;

fn read_float(address: u32) -> f32 {
    f32::from_bits(guest_memory::memory().u32_at(address))
}

fn read_word(address: u32) -> u32 {
    guest_memory::memory().u32_at(address)
}

// A route recorded by playing it, one point per line: x y z yaw pitch (older routes stop
// at yaw). Walking a route once and writing down where the player went gives a path that
// is, by construction, walkable; points pressed by hand at corners walked into walls,
// because they say nothing about the ground between them.
fn read_route_file(path: &[u8]) -> Route {
    let shown = c_string(path);
    let mut out = Route::new();
    let Ok(file) = fs::read(platform::path(&shown)) else {
        log_warn!("player: no route file at %s", shown.as_c_str());
        return out;
    };
    for line in scan::lines(&file, 256) {
        if matches!(line.as_bytes().first(), Some(b'#' | b'\n')) {
            continue;
        }
        let mut values = [0.0; 5];
        let mut word = [0u8; 8];
        let read = scan::word_and_reals(&line, &mut word, &mut values);
        let word = CStr::from_bytes_until_nul(&word).unwrap_or_default().to_bytes();
        if read == 6 && matches!(word, b"left" | b"fire") {
            let [x, y, _, yaw, pitch] = values;
            let act = if word[0] == b'l' { Act::Left } else { Act::Fire };
            out.push(Point { x, y, act, yaw, pitch, view: false });
            continue;
        }
        let read = scan::reals(&line, &mut values);
        if read >= 2 {
            let [x, y, _, yaw, pitch] = values;
            out.push(Point { x, y, act: Act::Walk, yaw, pitch, view: read == 5 });
        }
    }
    log_info!("player: route of %llu points read from %s", out.len() as u64, shown.as_c_str());
    out
}

// MW2_WALK_PATH=<file>[,<file>...]: every route named, in order. More than one because
// the map spawns each team at its own end, and a route recorded from one spawn starts
// three thousand units from the other: give a route per spawn, and the nearest is the
// one walked.
static ROUTES: LazyLock<Vec<Route>> = LazyLock::new(|| {
    let Some(text) = env::text(c"MW2_WALK_PATH") else { return Vec::new() };
    text.to_bytes()
        .split(|&byte| byte == b',')
        .filter(|path| !path.is_empty())
        .map(read_route_file)
        .filter(|route| !route.is_empty())
        .collect()
});

// The target, and how close counts as arrived.
#[derive(Copy, Clone)]
struct Target {
    set: bool,
    facing: bool, // a yaw to hold once there was given
    x: f32,
    y: f32,
    radius: f32,
    yaw: f32,
}

const NO_TARGET: Target = Target { set: false, facing: false, x: 0.0, y: 0.0, radius: 96.0, yaw: 0.0 };

// MW2_WALK_TO="x,y[,radius[,yaw]]".
static REQUESTED: LazyLock<Target> = LazyLock::new(|| {
    let mut t = NO_TARGET;
    let Some(text) = env::text(c"MW2_WALK_TO") else { return t };
    let (x, rest) = env::parse_real_from(text);
    t.x = x as f32;
    let Some(rest) = after_comma(rest) else { return t };
    let (y, rest) = env::parse_real_from(rest);
    t.y = y as f32;
    if let Some(rest) = after_comma(rest) {
        let (given, rest) = env::parse_real_from(rest);
        let given = given as f32;
        if given > 0.0 {
            t.radius = given;
        }
        if let Some(rest) = after_comma(rest) {
            t.yaw = env::parse_real_from(rest).0 as f32;
            t.facing = true;
        }
    }
    t.set = true;
    log_info!(
        "player: walking to %.1f %.1f, arriving within %.0f units, then %s",
        f64::from(t.x),
        f64::from(t.y),
        f64::from(t.radius),
        if t.facing { c"holding the yaw given" } else { c"turning on the spot" }
    );
    t
});

fn after_comma(text: &CStr) -> Option<&CStr> {
    (text.to_bytes().first() == Some(&b',')).then(|| &text[1..])
}

static TRACING: LazyLock<bool> = LazyLock::new(|| env::flag(c"MW2_TRACE_VIEWPOS"));
// MW2_WALK_REACH=<units>: how near counts. A route recorded with points closer together
// than this is cut short at every turn. A point with its view is walked straight at, so
// it can be held to closely.
static REACH: LazyLock<f32> = LazyLock::new(|| env::real(c"MW2_WALK_REACH", 0.0) as f32);
// MW2_WALK_PAUSE=<seconds>: stand still at every corner. A still camera is the only one
// a flash can be told apart from motion on.
static PAUSE: LazyLock<f64> = LazyLock::new(|| env::real(c"MW2_WALK_PAUSE", 0.0));
// A, and only ever A, is a safety property: see DismissMenus below. The one exception,
// asked for by name: MW2_TEAM_UP=<1 or 2> taps Up that many times on the team selection.
static TEAM_UPS: LazyLock<u32> = LazyLock::new(|| env::number(c"MW2_TEAM_UP", 0).min(2) as u32);

// The guard on the menu presses, whose cost when wrong is quitting the user's match:
// ground covered is the one test that separates "a menu is up" from "the player is
// walking", and past it nothing is pressed. A walk that wedged itself against a rock froze
// just like a menu, and was taken for one.
const PLAYING_AFTER: f32 = 300.0;
// A jump no one could have walked is a spawn or a respawn, not a step.
const TELEPORT: f32 = 1500.0;
const FAR: f32 = 1e30;
const DEGREES: f32 = 57.29578; // in a radian

// Times are 0 when not set.
struct State {
    // The route, which with recorded routes is only known once the player has spawned:
    // which one to walk, and where along it to join, both depend on where they came up.
    goal: Target,
    via: Route, // the corners before the goal
    route_chosen: bool,
    last_seen: Point, // for telling a respawn from a step
    seen_once: bool,
    leg: usize,
    leg_at: u64,

    // Progress on the current leg: the nearest the walk has managed, and when. Reset with
    // the leg, because the next corner is further away than the last one was close.
    best: f32,
    best_at: u64,
    escape_until: u64,
    escape_side: i32,
    escapes: u32,
    escape_angle: f32,

    // Menus. Before the player spawns the position reads a fixed placeholder and never
    // moves by so much as a float; a spawned player's never holds still, even standing.
    // So a position that has not moved at all is a menu waiting to be dismissed.
    still: Where,
    still_at: u64,
    pressing: bool,
    spawned: bool,
    playing: bool,
    // The team selection: the first menu met once the position has left the origin it
    // reads before anything is loaded -- the map's own camera is behind it.
    team_screen: u32,
    freezes: u32,
    // A player seen moving by a step: until then the sticks stay still, because on a menu
    // the left stick moves the selection.
    alive: bool,
    ups_sent: u32,
    up_window: u32, // MW2_TEAM_UP's taps, and the last one's window
    travelled: f32,

    // The action at the current leg: when it began (0 while walking to it), and when the
    // aim was first good.
    act_at: u64,
    aimed_at: u64,
    press_at: u64,

    ever_located: bool,
    joined_from: Point, // where the player stood when the route was chosen
    pause_until: u64,
    last_log: u64,
    closest: f32,
    last: Where,
}

const NOWHERE: Where = Where { x: 0.0, y: 0.0, z: 0.0, yaw: 0.0, pitch: 0.0 };

static STATE: Mutex<State> = Mutex::new(State {
    goal: NO_TARGET,
    via: Vec::new(),
    route_chosen: false,
    last_seen: Point::at(0.0, 0.0),
    seen_once: false,
    leg: 0,
    leg_at: 0,
    best: FAR,
    best_at: 0,
    escape_until: 0,
    escape_side: 1,
    escapes: 0,
    escape_angle: 90.0,
    still: NOWHERE,
    still_at: 0,
    pressing: false,
    spawned: false,
    playing: false,
    team_screen: 0,
    freezes: 0,
    alive: false,
    ups_sent: 0,
    up_window: u32::MAX,
    travelled: 0.0,
    act_at: 0,
    aimed_at: 0,
    press_at: 0,
    ever_located: false,
    joined_from: Point::at(0.0, 0.0),
    pause_until: 0,
    last_log: 0,
    closest: FAR,
    last: NOWHERE,
});

// Written by the thread that flies, read by the runtime's.
static ARRIVED: AtomicBool = AtomicBool::new(false);

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// The degrees still to turn, the short way round. The title's yaw is degrees
// counter-clockwise from +x, the same convention atan2 answers in.
fn shortest_turn(want: f32, have: f32) -> f32 {
    let mut turn = want - have;
    while turn > 180.0 {
        turn -= 360.0;
    }
    while turn < -180.0 {
        turn += 360.0;
    }
    turn
}

// The right stick deflection for a turn. It has to clear the title's own dead zone before
// it turns the view at all: a gentle correction is simply ignored, and the bot then walks
// past the target with its yaw frozen. Right stick to the positive side looks right,
// which counts yaw *down*, so the stick takes the opposite sign to the turn wanted -- the
// other way round the loop is stable at exactly the wrong heading.
fn turn_stick(degrees: f32) -> i16 {
    if degrees.abs() < 1.5 {
        return 0;
    }
    let want = (degrees / 45.0).clamp(-1.0, 1.0) * 28000.0;
    let least = 14000.0f32;
    (-(if want.abs() < least { least.copysign(want) } else { want })) as i16
}

// The pitch the right stick's Y has to push for. The title's pitch counts down as
// positive, and stick up looks up, so like the turn the stick takes the opposite sign.
fn pitch_stick(degrees: f32) -> i16 {
    turn_stick(degrees)
}

fn goal_of(g: &State) -> Target {
    if REQUESTED.set { *REQUESTED } else { g.goal }
}

fn start_leg(g: &mut State, now: u64, distance: f32) {
    g.leg_at = now;
    g.best = distance;
    g.best_at = now;
}

// Join a route at whichever of its points the player came up nearest, on whichever route
// that is: a spawn in the middle of a recorded route walks the rest of it rather than
// going back to the beginning. Chosen again after a respawn, which comes up somewhere
// else.
fn choose_route(g: &mut State, now: u64, x: f32, y: f32) {
    if ROUTES.is_empty() {
        return;
    }
    let (dx, dy) = (x - g.last_seen.x, y - g.last_seen.y);
    if g.seen_once && dx * dx + dy * dy > TELEPORT * TELEPORT {
        g.route_chosen = false;
    }
    g.last_seen = Point::at(x, y);
    g.seen_once = true;
    if g.route_chosen {
        return;
    }

    let (mut best_route, mut best_point, mut best) = (0, 0, FAR);
    for (r, route) in ROUTES.iter().enumerate() {
        for (i, point) in route.iter().enumerate() {
            let (px, py) = (point.x - x, point.y - y);
            if px * px + py * py < best {
                best = px * px + py * py;
                best_route = r;
                best_point = i;
            }
        }
    }
    g.via = ROUTES[best_route][best_point..].to_vec();
    // The route ends where the player stopped when it was recorded, unless MW2_WALK_TO
    // names somewhere else to go afterwards. A route that ends on something done there
    // keeps it as its last leg, and stands there afterwards facing the way it was done.
    if !REQUESTED.set {
        let end = g.via[g.via.len() - 1];
        g.goal = Target { set: true, x: end.x, y: end.y, ..NO_TARGET };
        if end.act == Act::Walk {
            g.via.pop();
        } else {
            g.goal.facing = true;
            g.goal.yaw = end.yaw;
        }
    }
    g.route_chosen = true;
    g.joined_from = Point::at(x, y);
    g.leg = 0;
    start_leg(g, now, FAR);
    let goal = goal_of(g);
    log_info!(
        "player: at %.1f %.1f -- route %llu of %llu, joining at point %llu (%.0f units away), %llu legs to %.1f %.1f",
        f64::from(x),
        f64::from(y),
        best_route as u64 + 1,
        ROUTES.len() as u64,
        best_point as u64,
        f64::from(best.sqrt()),
        g.via.len() as u64,
        f64::from(goal.x),
        f64::from(goal.y)
    );
}

// Tracks spawning and ground covered, and when the player has been frozen long enough to
// be looking at a menu, presses A. True when it took the controls this poll.
//
// A, and only ever A: a safety property, not a tactic. Ending the match takes START,
// which opens the pause menu, or a direction, which walks a selection onto "End Game"; a
// run that never sends either cannot quit the game, whatever screen it is on and however
// wrong its guess about which screen that is. A takes the team and class already
// selected, "Choose Class" in the in-game menu, and "No" on the End Game dialog.
//
// The one exception, asked for by name: MW2_TEAM_UP=<1 or 2> taps Up that many times on
// the team selection -- the first menu met once the map's camera is up -- before the A, so
// the run takes a team rather than Auto-assign, spawns on the same side every time and
// walks the same route. Nowhere else: the first menu is not one a direction can end the
// match from.
fn dismiss_menus(g: &mut State, now: u64, here: &Where, axes: &mut [i16; 4], press: &mut Press) -> bool {
    if here.x != g.still.x || here.y != g.still.y || here.z != g.still.z {
        if g.still_at != 0 {
            // The route chosen before this was chosen from the placeholder the position
            // reads until there is a player, and the first real movement is the spawn:
            // choose again from there.
            if !g.spawned && !ROUTES.is_empty() {
                g.route_chosen = false;
                choose_route(g, now, here.x, here.y);
            }
            g.spawned = true;
            let step = (here.x - g.still.x).hypot(here.y - g.still.y);
            if step < 500.0 {
                g.travelled += step;
            }
            // A player is there after a step, or after the jump off the team selection's
            // camera: before anything is loaded the position reads the origin, on the
            // team selection the map's camera, neither moves by a hair, and the jump from
            // the origin to that camera is the only one before the spawn.
            let from_origin = g.still.x == 0.0 && g.still.y == 0.0;
            if step < 500.0 || !from_origin {
                g.alive = true;
            }
            if !g.playing && g.travelled > PLAYING_AFTER {
                g.playing = true;
                log_info!(
                    "player: %.0f units covered -- in the world now, so no more menu presses for the rest of the run",
                    f64::from(g.travelled)
                );
            }
        }
        g.still = *here;
        g.still_at = now;
        if g.pressing {
            g.pressing = false;
            log_info!("player: moving again");
        }
        return false;
    }
    // How long a position has to hold before it is taken for a menu: a spawned player's
    // never holds for even a frame, so a few seconds is plenty, and every one of them is
    // waited through at every menu.
    let held = now.wrapping_sub(g.still_at);
    if held <= if g.spawned { 3 } else { 2 } * SECOND {
        return false;
    }

    if !g.pressing {
        g.pressing = true;
        g.freezes += 1;
        if g.team_screen == 0 && (here.x != 0.0 || here.y != 0.0) {
            g.team_screen = g.freezes;
        }
        log_info!(
            "player: frozen at %.1f %.1f %.1f -- pressing to get past whatever is waiting",
            f64::from(here.x),
            f64::from(here.y),
            f64::from(here.z)
        );
    }
    // The route starts when the player does.
    g.leg = 0;
    g.leg_at = now;
    // Sticks still while a menu is up: a physical pad left plugged in drifts, and a
    // drifting stick moves the selection under the press.
    *axes = [0; 4];
    // Slowly: a menu needs time to come up before the press meant for it arrives, and one
    // that arrives between two screens is lost.
    if (held / (600 * MILLISECOND)).is_multiple_of(2) {
        // Which press of this menu it is, counting from the first, in the same windows
        // the presses are held for. Taps counted as they are sent, not windows as they
        // pass: the first window can be half gone by the time the menu is noticed.
        let window = (held / (600 * MILLISECOND)) as u32 / 2;
        let team_screen = g.freezes == g.team_screen && !g.playing;
        let into = held % (1200 * MILLISECOND);
        if team_screen && g.ups_sent < *TEAM_UPS {
            // A tap, not the whole window: a direction held repeats and runs the
            // selection down the list.
            if into < 100 * MILLISECOND {
                *press = Press::Up;
                if window != g.up_window {
                    g.up_window = window;
                    g.ups_sent += 1;
                }
            } else {
                *press = Press::None;
            }
        } else if team_screen && g.ups_sent != 0 && window == g.up_window {
            *press = Press::None; // not the A in the last tap's window
        } else {
            *press = Press::A;
        }
    }
    true
}

// Moves on to the next corner once this one is reached -- generously, and on a timer,
// because a corner that cannot be got to must not hold up the rest of the route. Returns
// where to head for now.
fn next_corner(g: &mut State, here: &Where, goal: &Target, now: u64) -> Point {
    if g.leg_at == 0 {
        g.leg_at = now;
    }
    if g.leg >= g.via.len() {
        return Point::at(goal.x, goal.y);
    }
    let corner = g.via[g.leg];
    // An action is left by do_action, once done.
    if corner.act != Act::Walk {
        return corner;
    }
    let reach = (corner.x - here.x).hypot(corner.y - here.y);
    // A corner is there to get the route round a building, and leaving one early because
    // the target is nearer in a straight line walks into the building the corner was
    // named for.
    let near = if *REACH > 0.0 {
        *REACH
    } else if corner.view {
        24.0
    } else {
        140.0
    };
    // Gone by counts as well: a point missed by more than that is behind, and turning
    // back for it circles it.
    let from = if g.leg > 0 { g.via[g.leg - 1] } else { g.joined_from };
    let passed = (here.x - corner.x) * (corner.x - from.x) + (here.y - corner.y) * (corner.y - from.y) > 0.0;
    if reach >= near && !passed && now.wrapping_sub(g.leg_at) <= 90 * SECOND {
        return corner;
    }

    log_info!(
        "player: leaving corner %llu at %.1f %.1f, %.0f units from it",
        g.leg as u64,
        f64::from(here.x),
        f64::from(here.y),
        f64::from(reach)
    );
    g.leg += 1;
    start_leg(g, now, FAR);
    if *PAUSE > 0.0 {
        g.pause_until = now.wrapping_add((*PAUSE * 1e9) as u64);
    }
    if g.leg < g.via.len() { g.via[g.leg] } else { Point::at(goal.x, goal.y) }
}

fn action_name(act: Act) -> &'static CStr {
    if act == Act::Fire { c"fired" } else { c"pressed left" }
}

// At a recorded `left` or `fire`: get to the place -- closely for a shot, since the aim
// was taken from there -- stand, turn to the aim, press, and wait for the switch or the
// round's flight. A place that cannot be got to in twenty seconds is acted on from
// wherever the walk got. True while this holds the controls.
fn do_action(g: &mut State, here: &Where, now: u64, axes: &mut [i16; 4], press: &mut Press) -> bool {
    if g.leg >= g.via.len() || g.via[g.leg].act == Act::Walk {
        return false;
    }
    let at = g.via[g.leg];
    let reach = (at.x - here.x).hypot(at.y - here.y);
    let close = if at.act == Act::Fire { 24.0 } else { 96.0 };
    if g.act_at == 0 {
        if reach > close && now.wrapping_sub(g.leg_at) < 20 * SECOND {
            return false;
        }
        g.act_at = now;
        g.aimed_at = 0;
        g.press_at = 0;
        log_info!(
            "player: at %.1f %.1f %.1f, %.0f units from where the route %s -- aiming at yaw %.1f pitch %.1f",
            f64::from(here.x),
            f64::from(here.y),
            f64::from(here.z),
            f64::from(reach),
            action_name(at.act),
            f64::from(at.yaw),
            f64::from(at.pitch)
        );
    }
    axes[0] = 0;
    axes[1] = 0;
    // Both angles read 0..360 -- pitch 290 is 70 degrees up -- so both go the short way.
    let (yaw, pitch) = (shortest_turn(at.yaw, here.yaw), shortest_turn(at.pitch, here.pitch));
    axes[2] = turn_stick(yaw);
    axes[3] = pitch_stick(pitch);
    let aimed = at.act == Act::Left || (axes[2] == 0 && axes[3] == 0);
    if !aimed {
        g.aimed_at = 0;
    } else if g.aimed_at == 0 {
        g.aimed_at = now;
    }
    // Held on target a moment, so the view has stopped when the press comes; an aim that
    // will not settle is fired anyway after eight seconds.
    let settled = (g.aimed_at != 0 && now.wrapping_sub(g.aimed_at) > 300 * MILLISECOND)
        || now.wrapping_sub(g.act_at) > 8 * SECOND;
    if !settled {
        return true;
    }
    if g.press_at == 0 {
        g.press_at = now;
    }
    const PRESS: u64 = 300 * MILLISECOND;
    const AFTER: u64 = 2500 * MILLISECOND;
    let pressed = now.wrapping_sub(g.press_at);
    if pressed < PRESS {
        *press = if at.act == Act::Fire { Press::Fire } else { Press::Left };
        return true;
    }
    if pressed < PRESS + AFTER {
        return true;
    }
    log_info!(
        "player: %s at %.1f %.1f %.1f, yaw %.1f pitch %.1f (asked %.1f %.1f)",
        action_name(at.act),
        f64::from(here.x),
        f64::from(here.y),
        f64::from(here.z),
        f64::from(here.yaw),
        f64::from(here.pitch),
        f64::from(at.yaw),
        f64::from(at.pitch)
    );
    g.act_at = 0;
    g.press_at = 0;
    g.leg += 1;
    start_leg(g, now, FAR);
    true
}

// Progress is measured against the target, not the ground: a bot sliding along a rock
// face moves as fast as one walking and gets nowhere. Nothing gained in eight seconds
// means back off and go round -- alternating sides and widening the angle, so a place that
// defeats a sidestep gets a half-turn before another sidestep. Returns the bearing to walk
// on.
fn go_round(
    g: &mut State,
    bearing: f32,
    distance: f32,
    here: &Where,
    now: u64,
    backing: &mut bool,
    press: &mut Press,
) -> f32 {
    if distance < g.best - 40.0 {
        g.best = distance;
        g.best_at = now;
    }
    if g.best_at == 0 {
        g.best_at = now;
    }
    *backing = false;
    if g.escape_until > now {
        // Back off for the first part: turning on the spot against a rock and walking
        // again walks into the same rock.
        let left = g.escape_until - now;
        *backing = left > 2500 * MILLISECOND;
        if (left / (400 * MILLISECOND)).is_multiple_of(2) {
            *press = Press::A;
        }
        return bearing + g.escape_side as f32 * g.escape_angle;
    }
    if now.wrapping_sub(g.best_at) > 8 * SECOND {
        const ANGLES: [f32; 4] = [90.0, 135.0, 60.0, 170.0];
        g.escape_side = -g.escape_side;
        g.escape_angle = ANGLES[(g.escapes % 4) as usize];
        g.escapes = g.escapes.wrapping_add(1);
        g.escape_until = now + 4000 * MILLISECOND;
        g.best_at = now;
        g.best = distance;
        log_info!(
            "player: no closer than %.0f units for eight seconds at %.1f %.1f %.1f; backing off and going round to the %s at %.0f degrees",
            f64::from(distance),
            f64::from(here.x),
            f64::from(here.y),
            f64::from(here.z),
            if g.escape_side < 0 { c"left" } else { c"right" },
            f64::from(g.escape_angle)
        );
    }
    bearing
}

pub fn walking() -> bool {
    // A goal of its own comes only from a route.
    REQUESTED.set || !ROUTES.is_empty()
}

pub fn wanted() -> bool {
    walking() || *TRACING
}

pub fn arrived() -> bool {
    ARRIVED.load(Ordering::SeqCst)
}

/// How long the walk has been getting no nearer. Zero until the player has spawned and
/// started walking.
pub fn seconds_since_progress(now: u64) -> f64 {
    let g = state();
    if !g.spawned || !goal_of(&g).set || g.best_at == 0 {
        return 0.0;
    }
    now.wrapping_sub(g.best_at) as f64 / 1e9
}

// The route being recorded, shared by the thread that writes the places and the input
// poll that writes what was pressed at them.
static RECORD: Mutex<Option<File>> = Mutex::new(None);
static WAS_BUTTONS: AtomicU16 = AtomicU16::new(0);
static PULLED: AtomicBool = AtomicBool::new(false);

fn record() -> MutexGuard<'static, Option<File>> {
    RECORD.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn saw_pad(buttons: u16, right_trigger: u8) {
    const LEFT: u16 = 0x0004;
    let was = WAS_BUTTONS.swap(buttons, Ordering::Relaxed);
    let left = buttons & LEFT != 0 && was & LEFT == 0;
    let pull = right_trigger > 128;
    let fire = pull && !PULLED.swap(pull, Ordering::Relaxed);
    PULLED.store(pull, Ordering::Relaxed);
    if !left && !fire {
        return;
    }
    let Some(here) = locate() else { return };
    let (x, y, z) = (f64::from(here.x), f64::from(here.y), f64::from(here.z));
    let (yaw, pitch) = (f64::from(here.yaw), f64::from(here.pitch));
    log_info!(
        "player: %s at %.1f %.1f %.1f, yaw %.2f pitch %.2f",
        if fire { c"fired" } else { c"pressed left" },
        x,
        y,
        z,
        yaw,
        pitch
    );
    let what = if fire { c"fire" } else { c"left" };
    if let Some(file) = record().as_mut() {
        let _ = file.write_all(format_text!("%s %.1f %.1f %.1f %.2f %.2f\n", what, x, y, z, yaw, pitch).as_bytes());
    }
}

// A flight: where the camera was and when, twenty times a second, and the replay puts it
// back there with the title's own setviewpos every few milliseconds -- through the air and
// through walls, which no pad can be made to do, and to the same places at the same
// moments every run. Only where the title takes setviewpos, which the multiplayer one
// does not.
#[derive(Copy, Clone)]
struct Moment {
    time: f64,
    at: Where,
}

// The seconds count from the first poll that finds a player, in the recording and in the
// replay alike.
fn record_flight(path: &CStr) {
    let Ok(mut file) = File::create(platform::path(path)) else {
        log_error!("player: cannot write the flight to %s", path);
        return;
    };
    let _ = file.write_all(b"# seconds x y z yaw pitch -- recorded flight, replay with MW2_FLY_PATH\n");
    log_info!("player: recording the flight to %s", path);
    std::thread::spawn(move || {
        let mut from = 0;
        loop {
            std::thread::sleep(Duration::from_millis(50));
            let Some(here) = locate() else { continue };
            let now = clock::nanoseconds();
            if from == 0 {
                from = now;
            }
            let line = format_text!(
                "%.3f %.1f %.1f %.1f %.2f %.2f\n",
                (now - from) as f64 / 1e9,
                f64::from(here.x),
                f64::from(here.y),
                f64::from(here.z),
                f64::from(here.yaw),
                f64::from(here.pitch)
            );
            let _ = file.write_all(line.as_bytes());
        }
    });
}

fn between(a: f32, b: f32, part: f32) -> f32 {
    a + (b - a) * part
}

// Angles the short way round, so 350 to 10 passes through 0.
fn turned(a: f32, b: f32, part: f32) -> f32 {
    a + shortest_turn(b, a) * part
}

fn fly(path: &CStr) {
    let mut flight = Vec::new();
    if let Ok(file) = fs::read(platform::path(path)) {
        let (mut time, mut values) = (0.0, [0.0; 5]);
        for line in scan::lines(&file, 256) {
            if scan::moment(&line, &mut time, &mut values) {
                let [x, y, z, yaw, pitch] = values;
                flight.push(Moment { time, at: Where { x, y, z, yaw, pitch } });
            }
        }
    }
    if flight.len() < 2 {
        log_warn!("player: no flight to replay in %s", path);
        return;
    }
    let end = flight[flight.len() - 1];
    log_info!("player: flight of %llu moments over %.1f s read from %s", flight.len() as u64, end.time, path);
    std::thread::spawn(move || {
        while locate().is_none() {
            std::thread::sleep(Duration::from_millis(5));
        }
        let from = clock::nanoseconds();
        log_info!("player: the flight starts");
        let mut i = 0;
        loop {
            let t = (clock::nanoseconds() - from) as f64 / 1e9;
            if t >= end.time {
                break;
            }
            while flight[i + 1].time <= t {
                i += 1;
            }
            let (a, b) = (flight[i], flight[i + 1]);
            let part = ((t - a.time) / (b.time - a.time)) as f32;
            console::place(&format_text!(
                "setviewpos %.1f %.1f %.1f %.2f %.2f",
                f64::from(between(a.at.x, b.at.x, part)),
                f64::from(between(a.at.y, b.at.y, part)),
                // The title's teleport lifts whoever it moves by one unit.
                f64::from(between(a.at.z, b.at.z, part) - 1.0),
                f64::from(turned(a.at.yaw, b.at.yaw, part)),
                f64::from(turned(a.at.pitch, b.at.pitch, part))
            ));
            std::thread::sleep(Duration::from_millis(4));
        }
        if let Some(here) = locate() {
            log_info!(
                "player: the flight is over, at %.1f %.1f %.1f yaw %.1f pitch %.1f; it was recorded ending at %.1f %.1f %.1f yaw %.1f pitch %.1f",
                f64::from(here.x),
                f64::from(here.y),
                f64::from(here.z),
                f64::from(here.yaw),
                f64::from(here.pitch),
                f64::from(end.at.x),
                f64::from(end.at.y),
                f64::from(end.at.z),
                f64::from(end.at.yaw),
                f64::from(end.at.pitch)
            );
        }
        ARRIVED.store(true, Ordering::SeqCst);
    });
}

// Writing down a route while someone walks it, on a thread of its own, so nothing about
// the game's input or timing changes. A point is kept when the player has moved far enough
// from the last one to be a separate leg: closer and the replay spends its time declaring
// legs reached, further and it cuts corners through walls.
pub fn start_recording() {
    if let Some(flight) = env::text(c"MW2_RECORD_FLIGHT") {
        record_flight(flight);
    }
    if let Some(flight) = env::text(c"MW2_FLY_PATH") {
        fly(flight);
    }
    let Some(path) = env::text(c"MW2_RECORD_PATH") else { return };
    let spacing = env::real(c"MW2_RECORD_SPACING", 110.0) as f32;
    let Ok(mut file) = File::create(platform::path(path)) else {
        log_error!("player: cannot write the route to %s", path);
        return;
    };
    let _ = file.write_all(
        b"# x y z yaw pitch -- recorded route, replay with MW2_WALK_PATH; `left`/`fire` lines are presses\n",
    );
    let header = file.stream_position().unwrap_or(0);
    *record() = Some(file);
    log_info!("player: recording the route to %s, a point every %.0f units", path, f64::from(spacing));
    std::thread::spawn(move || {
        let mut last = Where::default();
        let mut have = false;
        let mut kept = 0u64;
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let Some(here) = locate() else { continue };
            let (dx, dy) = (here.x - last.x, here.y - last.y);
            let square = dx * dx + dy * dy;
            if have && square < spacing * spacing {
                continue;
            }
            let mut held = record();
            let Some(file) = held.as_mut() else { continue };
            // The route starts at the spawn: what came before it is the menu, or a life
            // that ended.
            if have && square > TELEPORT * TELEPORT {
                log_info!(
                    "player: spawned at %.1f %.1f -- the route starts here",
                    f64::from(here.x),
                    f64::from(here.y)
                );
                let _ = file.seek(SeekFrom::Start(header));
                let _ = file.set_len(header); // best effort
                kept = 0;
            }
            let line = format_text!(
                "%.1f %.1f %.1f %.1f %.1f\n",
                f64::from(here.x),
                f64::from(here.y),
                f64::from(here.z),
                f64::from(here.yaw),
                f64::from(here.pitch)
            );
            let _ = file.write_all(line.as_bytes()); // a run that ends in a crash still leaves its route
            last = here;
            have = true;
            kept += 1;
            if kept.is_multiple_of(25) {
                log_info!("player: %llu route points so far", kept);
            }
        }
    });
}

/// None before the player exists -- the menus, the loading screen, and the seconds
/// between spawning and the first snapshot.
pub fn locate() -> Option<Where> {
    // Not derived for this title.
    #[allow(clippy::absurd_extreme_comparisons)]
    if T_DATA_ClientStates == 0 {
        return None;
    }
    // The title's own viewpos walks a local-client index into a table of slots and
    // multiplies by the state size. The index reads -1 here for the length of a match and
    // the answer still comes out right, because a client with one local player is slot
    // zero and zero is where the array starts -- so the indirection is not worth
    // reproducing.
    #[cfg(T_CLIENT_STATES_ARE_HERE)]
    let client = T_DATA_ClientStates;
    #[cfg(not(T_CLIENT_STATES_ARE_HERE))]
    let client = read_word(T_DATA_ClientStates);
    if client == 0 {
        return None;
    }
    // The same field viewpos checks before printing: zero until there is a player.
    if read_word(client.wrapping_add(T_CLIENT_VALID)) == 0 {
        return None;
    }
    let origin = client.wrapping_add(T_CLIENT_ORIGIN);
    let angles = client.wrapping_add(T_CLIENT_ANGLES);
    let out = Where {
        x: read_float(origin),
        y: read_float(origin.wrapping_add(4)),
        z: read_float(origin.wrapping_add(8)),
        pitch: read_float(angles),
        yaw: read_float(angles.wrapping_add(4)),
    };
    // A state being written as it is read, or one that has never been written.
    if !out.x.is_finite() || !out.y.is_finite() || !out.yaw.is_finite() {
        return None;
    }
    (out.x.abs() < 1e6 && out.y.abs() < 1e6).then_some(out)
}

/// `axes` is the guest's stick state, taken over only while there is somewhere to go and
/// the player has been located, and `press` the button to hold this poll. True if it
/// wrote anything.
pub fn steer(now: u64, axes: &mut [i16; 4], press: &mut Press) -> bool {
    *press = Press::None;
    if !wanted() {
        return false;
    }
    let Some(here) = locate() else { return false };
    let mut guard = state();
    let g = &mut *guard;
    choose_route(g, now, here.x, here.y);
    let mut goal = goal_of(g);
    if !goal.set && !*TRACING {
        return false;
    }
    g.ever_located = true;
    g.last = here;

    // Arrival before the menu check: a bot that has arrived and is holding still would
    // otherwise be taken for a menu and press.
    let standing = g.spawned && (goal.x - here.x).hypot(goal.y - here.y) <= goal.radius;
    // Nor while aiming: a player standing still to take a shot is not a menu.
    let acting = g.act_at != 0;
    if !standing && !acting && dismiss_menus(g, now, &here, axes, press) {
        return true;
    }
    // What the spawn chose in there.
    goal = goal_of(g);
    if !g.alive {
        // The team selection shows the map's camera, which reads like a player standing
        // somewhere: steering then ran the selection up the list.
        *axes = [0; 4];
        return true;
    }

    if *TRACING && now.wrapping_sub(g.last_log) > SECOND {
        g.last_log = now;
        log_info!(
            "player: at %.1f %.1f %.1f, yaw %.1f pitch %.1f",
            f64::from(here.x),
            f64::from(here.y),
            f64::from(here.z),
            f64::from(here.yaw),
            f64::from(here.pitch)
        );
    }
    if !goal.set {
        return false;
    }

    let to = next_corner(g, &here, &goal, now);
    if now < g.pause_until {
        // Standing, and not counted as getting nowhere.
        *axes = [0; 4];
        g.leg_at = now;
        g.best_at = now;
        return true;
    }
    if do_action(g, &here, now, axes, press) {
        return true;
    }

    let (dx, dy) = (to.x - here.x, to.y - here.y);
    let distance = (dx * dx + dy * dy).sqrt();
    // Only once there is a player: before the spawn the position is a fixed point, and its
    // distance is not an approach the run made.
    let to_goal = (goal.x - here.x).hypot(goal.y - here.y);
    if g.spawned && to_goal < g.closest {
        g.closest = to_goal;
    }

    if g.leg >= g.via.len() && distance <= goal.radius {
        if !ARRIVED.load(Ordering::SeqCst) {
            ARRIVED.store(true, Ordering::SeqCst);
            log_info!(
                "player: arrived at %.1f %.1f %.1f, %.0f units from the target",
                f64::from(here.x),
                f64::from(here.y),
                f64::from(here.z),
                f64::from(distance)
            );
        }
        // With a yaw given, hold it; without one, turn slowly and forever, so one run sees
        // the whole horizon from here.
        axes[0] = 0;
        axes[1] = 0;
        axes[3] = 0;
        axes[2] = if goal.facing { turn_stick(shortest_turn(goal.yaw, here.yaw)) } else { 6000 };
        return true;
    }
    ARRIVED.store(false, Ordering::SeqCst);

    let mut backing = false;
    let bearing = go_round(g, dy.atan2(dx) * DEGREES, distance, &here, now, &mut backing, press);
    let turn = shortest_turn(bearing, here.yaw);
    // Turn hard when far off and walk whenever the target is anywhere ahead: turning while
    // walking follows a curve, which gets out of the corners that turning on the spot gets
    // stuck in.
    axes[2] = turn_stick(turn);
    axes[1] = if backing {
        -30000
    } else if turn.abs() < 60.0 {
        32000
    } else {
        12000
    };
    // Slowly onto the spot a shot was taken from: at a run the walk circles a point it has
    // to stand within a couple of feet of.
    if !backing && g.leg < g.via.len() && g.via[g.leg].act != Act::Walk && distance < 150.0 {
        axes[1] = axes[1].min(12000);
    }
    axes[0] = 0;
    axes[3] = 0;
    // A point recorded with its view: look where the player looked and move straight at
    // the point with the left stick, whichever way that is from the view. Turning to face
    // each point instead swings wide of it, and the walk then meets what the player went
    // round. Not while going round something itself, which steers by turning.
    if to.view && !backing && g.escape_until <= now {
        let off = shortest_turn(dy.atan2(dx) * DEGREES, here.yaw) / DEGREES;
        axes[0] = (-32000.0 * off.sin()) as i16;
        axes[1] = (32000.0 * off.cos()) as i16;
        axes[2] = turn_stick(shortest_turn(to.yaw, here.yaw));
        axes[3] = pitch_stick(shortest_turn(to.pitch, here.pitch));
    }
    true
}

/// For the shutdown report: whether the target was reached, and where the run actually
/// spent its time. It may be running because of a fault in the middle of a poll, so it
/// does not wait for the lock for long.
pub fn report() {
    let Some(g) = crate::runtime::lock_for_report(&STATE) else { return };
    if !goal_of(&g).set && !*TRACING {
        return;
    }
    if !g.ever_located {
        log_info!("player: never located -- the run did not reach the world");
        return;
    }
    log_info!(
        "player: last at %.1f %.1f %.1f, yaw %.1f; closest approach %.0f units, %u times going round something%s",
        f64::from(g.last.x),
        f64::from(g.last.y),
        f64::from(g.last.z),
        f64::from(g.last.yaw),
        f64::from(if g.closest == FAR { 0.0 } else { g.closest }),
        g.escapes,
        if arrived() { c" (arrived)" } else { c"" }
    );
}
