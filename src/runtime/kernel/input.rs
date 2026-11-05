// XamInput* -- gamepad state, from SDL3.

use crate::guest::{Memory, Register, kernel_import};
use crate::log::{c_string, format_text, log_info, log_warn};
#[cfg(feature = "diagnostics")]
use crate::runtime::{bridge, clock, pacing_trace, player, stutters};
use crate::runtime::{console, friends, guide, online, signin};
use sdl::{Axis, Button, Pad};
use std::ffi::CStr;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

const ERROR_SUCCESS: u32 = 0;
const ERROR_BAD_ARGUMENTS: u32 = 0x00A0;
const ERROR_DEVICE_NOT_CONNECTED: u32 = 0x048F;

// What of SDL this asks: the pads, the keyboard and the hints.
#[allow(unsafe_code)]
mod sdl {
    use crate::log::log_warn;
    use sdl3_sys::everything as sdl;
    use std::ffi::{CStr, CString, c_char};
    use std::sync::Once;

    pub use sdl::{SDL_GamepadAxis as Axis, SDL_GamepadButton as Button, SDL_Scancode as Scancode};

    /// An open gamepad. SDL's gamepad calls may come from any thread.
    #[derive(Copy, Clone, PartialEq, Eq)]
    pub struct Pad(*mut sdl::SDL_Gamepad);
    unsafe impl Send for Pad {}

    // SDL's strings live until the device goes or the call is made again, so each is
    // copied when it is read.
    fn text(pointer: *const c_char) -> Option<CString> {
        (!pointer.is_null()).then(|| unsafe { CStr::from_ptr(pointer) }.to_owned())
    }

    pub fn error() -> CString {
        text(sdl::SDL_GetError()).unwrap_or_default()
    }

    pub fn start() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            if !unsafe { sdl::SDL_InitSubSystem(sdl::SDL_INIT_GAMEPAD) } {
                log_warn!("SDL gamepad init failed: %s", error().as_c_str());
            }
        });
    }

    // An SDL list, copied out and freed.
    fn list(get: unsafe extern "C" fn(*mut i32) -> *mut sdl::SDL_JoystickID) -> Option<Vec<sdl::SDL_JoystickID>> {
        let mut count = 0;
        let ids = unsafe { get(&mut count) };
        if ids.is_null() {
            return None;
        }
        let copied = unsafe { std::slice::from_raw_parts(ids, count.max(0) as usize) }.to_vec();
        unsafe { sdl::SDL_free(ids.cast()) };
        Some(copied)
    }

    /// What is plugged in: each joystick's name, vendor, product and whether SDL has a
    /// gamepad layout for it.
    pub fn joysticks() -> Vec<(Option<CString>, u16, u16, bool)> {
        let ids = list(sdl::SDL_GetJoysticks).unwrap_or_default();
        ids.into_iter()
            .map(|id| unsafe {
                (
                    text(sdl::SDL_GetJoystickNameForID(id)),
                    sdl::SDL_GetJoystickVendorForID(id),
                    sdl::SDL_GetJoystickProductForID(id),
                    sdl::SDL_IsGamepad(id),
                )
            })
            .collect()
    }

    pub const IGNORE_HINTS: [&CStr; 2] =
        [c"SDL_GAMECONTROLLER_IGNORE_DEVICES", c"SDL_GAMECONTROLLER_IGNORE_DEVICES_EXCEPT"];

    pub fn hint(name: &CStr) -> Option<CString> {
        text(unsafe { sdl::SDL_GetHint(name.as_ptr()) })
    }

    /// The `index`th gamepad SDL knows, opened; None when there are not that many, or
    /// SDL has no list.
    pub fn open(index: usize) -> Option<Pad> {
        let ids = list(sdl::SDL_GetGamepads)?;
        let id = *ids.get(index)?;
        let pad = unsafe { sdl::SDL_OpenGamepad(id) };
        (!pad.is_null()).then_some(Pad(pad))
    }

    impl Pad {
        pub fn connected(self) -> bool {
            unsafe { sdl::SDL_GamepadConnected(self.0) }
        }

        pub fn close(self) {
            unsafe { sdl::SDL_CloseGamepad(self.0) }
        }

        pub fn name(self) -> Option<CString> {
            text(unsafe { sdl::SDL_GetGamepadName(self.0) })
        }

        pub fn button(self, button: Button) -> bool {
            unsafe { sdl::SDL_GetGamepadButton(self.0, button) }
        }

        pub fn axis(self, axis: Axis) -> i16 {
            unsafe { sdl::SDL_GetGamepadAxis(self.0, axis) }
        }

        pub fn rumble(self, low: u16, high: u16, milliseconds: u32) -> bool {
            unsafe { sdl::SDL_RumbleGamepad(self.0, low, high, milliseconds) }
        }
    }

    pub fn update() {
        unsafe { sdl::SDL_UpdateGamepads() }
    }

    /// Whether each of `keys` is down; all up while there is no window to read them from.
    pub fn keys_down(keys: &[Scancode]) -> Option<Vec<bool>> {
        if unsafe { sdl::SDL_WasInit(sdl::SDL_INIT_VIDEO) }.0 == 0 {
            return None;
        }
        let mut count = 0;
        let state = unsafe { sdl::SDL_GetKeyboardState(&mut count) };
        if state.is_null() {
            return None;
        }
        let state = unsafe { std::slice::from_raw_parts(state, count.max(0) as usize) };
        Some(keys.iter().map(|key| state.get(key.0 as usize).copied().unwrap_or(false)).collect())
    }
}

const DPAD_UP: u16 = 0x0001;
const DPAD_DOWN: u16 = 0x0002;
const DPAD_LEFT: u16 = 0x0004;
const DPAD_RIGHT: u16 = 0x0008;
const START: u16 = 0x0010;
const BACK: u16 = 0x0020;
const LEFT_THUMB: u16 = 0x0040;
const RIGHT_THUMB: u16 = 0x0080;
const LEFT_SHOULDER: u16 = 0x0100;
const RIGHT_SHOULDER: u16 = 0x0200;
const A: u16 = 0x1000;
const B: u16 = 0x2000;
const X: u16 = 0x4000;
const Y: u16 = 0x8000;

// The pads at each controller. The title reads them on its main thread and sets the
// motors from whichever thread runs its rumble; both open and close pads here.
struct Pads {
    open: [Option<Pad>; 4],
    looked: [Option<Instant>; 4],
    said: Option<Vec<u8>>, // what is plugged in, as last said
}

static PADS: Mutex<Pads> = Mutex::new(Pads { open: [None; 4], looked: [None; 4], said: None });

fn pads() -> MutexGuard<'static, Pads> {
    PADS.lock().unwrap_or_else(PoisonError::into_inner)
}

// What is plugged in, said whenever it changes: a controller the game does not answer to
// is either not here at all, here as a joystick SDL has no layout for, or one SDL was
// told to ignore.
fn say_devices(said: &mut Option<Vec<u8>>) {
    let mut now = Vec::new();
    for (i, (name, vendor, product, gamepad)) in sdl::joysticks().into_iter().enumerate() {
        let mut one = format_text!(
            "%s%s (%04X:%04X, %s)",
            if i != 0 { c", " } else { c"" },
            name.as_deref().unwrap_or(c"unnamed"),
            u32::from(vendor),
            u32::from(product),
            if gamepad { c"a gamepad" } else { c"no gamepad layout known" }
        )
        .into_bytes();
        one.truncate(159); // as the C++'s line of 160
        now.extend_from_slice(&one);
    }
    if said.as_ref() == Some(&now) {
        return;
    }
    if now.is_empty() {
        log_info!("input: no controller is plugged in, as far as SDL sees");
    } else {
        log_info!("input: plugged in: %s", c_string(now.as_slice()).as_c_str());
    }
    *said = Some(now);
    for hint in sdl::IGNORE_HINTS {
        if let Some(value) = sdl::hint(hint).filter(|value| !value.is_empty()) {
            log_info!("input: SDL is told %s=%s", hint, value.as_c_str());
        }
    }
}

fn pad_for(user: u32) -> Option<Pad> {
    if user >= 4 {
        return None;
    }
    let user = user as usize;
    sdl::start();
    let mut pads = pads();
    if let Some(pad) = pads.open[user].filter(|pad| !pad.connected()) {
        pad.close();
        pads.open[user] = None;
    }
    // Looked for once a second, not on every poll: the title polls all four users every
    // frame, and each look allocates the device list.
    let now = Instant::now();
    if pads.open[user].is_none() && pads.looked[user].is_none_or(|looked| now - looked >= Duration::from_secs(1)) {
        pads.looked[user] = Some(now);
        if user == 0 {
            say_devices(&mut pads.said);
        }
        pads.open[user] = sdl::open(user);
        if let Some(pad) = pads.open[user] {
            log_info!("input: controller %u is %s", user as u32, pad.name().as_deref().unwrap_or(c"unnamed"));
        }
    }
    pads.open[user]
}

const BUTTONS: [(Button, u16); 14] = [
    (Button::DPAD_UP, DPAD_UP),
    (Button::DPAD_DOWN, DPAD_DOWN),
    (Button::DPAD_LEFT, DPAD_LEFT),
    (Button::DPAD_RIGHT, DPAD_RIGHT),
    (Button::START, START),
    (Button::BACK, BACK),
    (Button::LEFT_STICK, LEFT_THUMB),
    (Button::RIGHT_STICK, RIGHT_THUMB),
    (Button::LEFT_SHOULDER, LEFT_SHOULDER),
    (Button::RIGHT_SHOULDER, RIGHT_SHOULDER),
    (Button::SOUTH, A),
    (Button::EAST, B),
    (Button::WEST, X),
    (Button::NORTH, Y),
];

// The keyboard, for when there is no controller. Only useful with the window open, since
// that is what gives SDL a focused surface to read keys from.
const KEYS: [(sdl::Scancode, u16); 13] = [
    (sdl::Scancode::RETURN, START),
    (sdl::Scancode::ESCAPE, BACK),
    (sdl::Scancode::UP, DPAD_UP),
    (sdl::Scancode::DOWN, DPAD_DOWN),
    (sdl::Scancode::LEFT, DPAD_LEFT),
    (sdl::Scancode::RIGHT, DPAD_RIGHT),
    (sdl::Scancode::Z, A),
    (sdl::Scancode::X, B),
    (sdl::Scancode::C, X),
    (sdl::Scancode::V, Y),
    (sdl::Scancode::Q, LEFT_SHOULDER),
    (sdl::Scancode::E, RIGHT_SHOULDER),
    (sdl::Scancode::HOME, signin::GUIDE),
];

fn keyboard_buttons() -> u16 {
    let codes = KEYS.map(|(code, _)| code);
    let Some(down) = sdl::keys_down(&codes) else { return 0 };
    KEYS.iter().zip(down).filter(|(_, down)| *down).fold(0, |buttons, ((_, bit), _)| buttons | bit)
}

// MW2_INPUT_SCRIPT="2.5:start,6:a" presses buttons at fixed times, so a title can be
// driven to a screen without a hand on the controller, and a bring-up run reproduces the
// same path twice.
//
// An entry may name `lt` or `rt` for a trigger rather than a button, or a stick direction
// -- `lx+ lx- ly+ ly- rx+ rx- ry+ ry-` -- and may carry a third field giving how long to
// hold it: "40:lt:25" aims down the sights for twenty-five seconds. Some of what the
// renderer does only happens while a trigger is held -- MW2 turns its depth of field on
// when the player aims -- and some screens only respond to a stick: the first-boot
// brightness screen will not accept anything until the slider has been moved, so a run
// that only presses buttons sits on it forever. A name ending in `@2`, `@3` or `@4` is
// that player's controller, which the script then stands in for: "5:a@2" is the second
// player's A.
#[derive(Copy, Clone)]
enum Pressed {
    Button(u16),
    Trigger(usize),
    Axis(usize, i16),
}

#[derive(Copy, Clone)]
struct Press {
    at: f64,
    pressed: Pressed,
    held: f64,
    user: usize,
}

const NAMED_BUTTONS: [(&[u8], u16); 15] = [
    (b"up", DPAD_UP),
    (b"down", DPAD_DOWN),
    (b"left", DPAD_LEFT),
    (b"right", DPAD_RIGHT),
    (b"start", START),
    (b"back", BACK),
    (b"lthumb", LEFT_THUMB),
    (b"rthumb", RIGHT_THUMB),
    (b"lb", LEFT_SHOULDER),
    (b"rb", RIGHT_SHOULDER),
    (b"a", A),
    (b"b", B),
    (b"x", X),
    (b"y", Y),
    (b"guide", signin::GUIDE),
];

// "lx+" is the left stick's X pushed fully one way, "ly-" the other.
fn named_axis(name: &[u8]) -> Option<(usize, i16)> {
    let [stick, which, sign] = *name else { return None };
    let value = match sign {
        b'+' => 32767,
        b'-' => -32768,
        _ => return None,
    };
    let stick = match stick {
        b'l' => 0,
        b'r' => 2,
        _ => return None,
    };
    let which = match which {
        b'x' => 0,
        b'y' => 1,
        _ => return None,
    };
    Some((stick + which, value))
}

fn real(text: &[u8]) -> f64 {
    crate::env::parse_real(&c_string(text))
}

fn parse_script(text: &[u8]) -> Vec<Press> {
    let mut script = Vec::new();
    for entry in text.split(|&byte| byte == b',') {
        let Some(colon) = entry.iter().position(|&byte| byte == b':') else { continue };
        let at = real(&entry[..colon]);
        let mut name = &entry[colon + 1..];
        let mut held = 0.2;
        if let Some(second) = name.iter().position(|&byte| byte == b':') {
            held = real(&name[second + 1..]);
            if held <= 0.0 {
                held = 0.2;
            }
            name = &name[..second];
        }
        let mut user = 0;
        if name.len() > 2 && name[name.len() - 2] == b'@' {
            user = usize::from(name[name.len() - 1].wrapping_sub(b'1') & 3);
            name = &name[..name.len() - 2];
        }
        let pressed = match name {
            b"lt" => Some(Pressed::Trigger(0)),
            b"rt" => Some(Pressed::Trigger(1)),
            _ => named_axis(name).map(|(axis, value)| Pressed::Axis(axis, value)).or_else(|| {
                NAMED_BUTTONS.iter().find(|(named, _)| *named == name).map(|&(_, bit)| Pressed::Button(bit))
            }),
        };
        if let Some(pressed) = pressed {
            script.push(Press { at, pressed, held, user });
        }
    }
    script
}

// The script as read, and what it holds at each controller as of the last poll there.
struct Script {
    presses: Vec<Press>,
    scripted: [bool; 4], // the script presses something on this controller
    start: Instant,
    triggers: [[u8; 2]; 4],
    axes: [[i16; 4]; 4],
    axis_held: [[bool; 4]; 4],
}

static SCRIPT: Mutex<Option<Script>> = Mutex::new(None);

fn script() -> MutexGuard<'static, Option<Script>> {
    SCRIPT.lock().unwrap_or_else(PoisonError::into_inner)
}

fn scripted(user: usize) -> bool {
    script().as_ref().is_some_and(|script| script.scripted[user])
}

fn read_script() -> Script {
    let text = if cfg!(feature = "diagnostics") { crate::env::text(c"MW2_INPUT_SCRIPT") } else { None };
    let presses = text.map(|text| parse_script(text.to_bytes())).unwrap_or_default();
    let mut scripted = [false; 4];
    for press in &presses {
        scripted[press.user] = true;
    }
    const AXIS_NAMES: [&CStr; 4] = [c"left stick X", c"left stick Y", c"right stick X", c"right stick Y"];
    for press in &presses {
        let (what, bit) = match press.pressed {
            Pressed::Axis(axis, _) => (AXIS_NAMES[axis], 0),
            Pressed::Button(bit) => (c"press", bit),
            Pressed::Trigger(1) => (c"right trigger", 0),
            Pressed::Trigger(_) => (c"left trigger", 0),
        };
        log_info!(
            "input: controller %u, scripted %s at %.2f s for %.2f s (%04X)",
            press.user as u32,
            what,
            press.at,
            press.held,
            u32::from(bit)
        );
    }
    Script {
        presses,
        scripted,
        start: Instant::now(),
        triggers: [[0; 2]; 4],
        axes: [[0; 4]; 4],
        axis_held: [[false; 4]; 4],
    }
}

// What the script presses at a controller now, its triggers and sticks kept for the poll.
fn scripted_buttons(user: usize) -> u16 {
    if !cfg!(feature = "diagnostics") {
        return 0;
    }
    let mut kept = script();
    let script = kept.get_or_insert_with(read_script);
    script.triggers[user] = [0; 2];
    script.axes[user] = [0; 4];
    script.axis_held[user] = [false; 4];
    if !script.scripted[user] {
        return 0;
    }
    // A press lasts long enough for a title polling at 60 Hz to see it and short enough
    // not to repeat, unless the entry asked to hold it.
    let now = script.start.elapsed().as_secs_f64();
    let mut buttons = 0;
    for press in &script.presses {
        if press.user != user || now < press.at || now >= press.at + press.held {
            continue;
        }
        match press.pressed {
            Pressed::Axis(axis, value) => {
                script.axes[user][axis] = value;
                script.axis_held[user][axis] = true;
            }
            Pressed::Button(bit) => buttons |= bit,
            Pressed::Trigger(trigger) => script.triggers[user][trigger] = 255,
        }
    }
    buttons
}

// A scripted stick entry moves nothing on its own, so a run with no pad attached still
// has to take the path that applies it.
fn scripted_axes_held(user: usize) -> bool {
    script().as_ref().is_some_and(|script| script.axis_held[user].contains(&true))
}

// The packet number tells the title whether anything has changed since it last looked.
// Incrementing it every poll says "changed" sixty times a second.
#[derive(Copy, Clone, Default)]
struct Latch {
    packet: u32,
    buttons: u16,
    triggers: [u8; 2],
    axes: [i16; 4],
}

static LATCHES: Mutex<[Latch; 4]> = Mutex::new([Latch { packet: 0, buttons: 0, triggers: [0; 2], axes: [0; 4] }; 4]);

fn player_wanted() -> bool {
    #[cfg(feature = "diagnostics")]
    return player::wanted();
    #[cfg(not(feature = "diagnostics"))]
    false
}

// A flash hunt (MW2_FLASH_HUNT, tools/flash_hunt.sh) takes the pad's Y as its mark,
// because reaching for F7 mid-fight is not going to happen. The title does not see it: Y
// switches weapon, and the switch would change the very frames the mark keeps. A stutter
// hunt (MW2_STUTTERS) marks a hitch with it the same way.
#[cfg(feature = "diagnostics")]
fn hunt_mark(pad: Pad, buttons: &mut u16) {
    use std::sync::LazyLock;
    use std::sync::atomic::{AtomicBool, Ordering};
    static HUNTING: LazyLock<bool> = LazyLock::new(|| crate::env::flag(c"MW2_FLASH_HUNT") || stutters::on());
    static WAS_DOWN: AtomicBool = AtomicBool::new(false);
    if !*HUNTING {
        return;
    }
    let down = pad.button(Button::NORTH);
    if down && !WAS_DOWN.load(Ordering::Relaxed) {
        if stutters::on() {
            stutters::mark(pacing_trace::microseconds(clock::nanoseconds()), c"pad Y");
        } else {
            bridge::flash_seen(c"pad Y");
        }
    }
    WAS_DOWN.store(down, Ordering::Relaxed);
    *buttons &= !Y;
}

// What a person did, before the autopilot adds to it: a recorded route keeps the presses
// (MW2_RECORD_PATH), and every shot is logged.
#[cfg(feature = "diagnostics")]
fn autopilot(buttons: &mut u16, triggers: &mut [u8; 2], axes: &mut [i16; 4]) {
    player::saw_pad(*buttons, triggers[1]);
    let mut press = player::Press::None;
    if player::steer(clock::nanoseconds(), axes, &mut press) {
        match press {
            player::Press::A => *buttons |= A,
            player::Press::Up => *buttons |= DPAD_UP,
            player::Press::Left => *buttons |= DPAD_LEFT,
            player::Press::Fire => triggers[1] = 255,
            player::Press::None => {}
        }
    }
}

// XINPUT_STATE: the packet number, then XINPUT_GAMEPAD: buttons, the two triggers and
// the four stick axes.
fn store_state(memory: Memory, state: u32, latch: &Latch) {
    memory.store_u32(state, latch.packet);
    memory.store_u16(state + 4, latch.buttons);
    memory.store_u8(state + 6, latch.triggers[0]);
    memory.store_u8(state + 7, latch.triggers[1]);
    for (i, axis) in (0u32..).zip(latch.axes) {
        memory.store_u16(state + 8 + i * 2, axis as u16);
    }
}

kernel_import!(XamInputGetState, |ctx, memory| {
    // The title polls input once a frame from its main thread, which makes this the
    // cheapest place to hand the engine a scripted console command.
    online::frame();
    console::pump(ctx, memory);

    let (user, state) = (ctx.r3.as_u32(), ctx.r5.as_u32());
    if state == 0 {
        ctx.r3.u64 = u64::from(ERROR_DEVICE_NOT_CONNECTED);
        return;
    }
    if user >= 4 {
        store_state(memory, state, &Latch::default());
        ctx.r3.u64 = u64::from(ERROR_DEVICE_NOT_CONNECTED);
        return;
    }
    let pad = pad_for(user);
    let at = user as usize;
    let elsewhere = if user == 0 { keyboard_buttons() } else { 0 } | scripted_buttons(at);
    let script_stands_in = if user != 0 { scripted(at) } else { scripted_axes_held(0) || player_wanted() };
    if pad.is_none() && elsewhere == 0 && !script_stands_in {
        // Nothing is held at a controller that is not there -- the keyboard's with every
        // key up: the screens hear it, so that their next press of a key counts as one.
        friends::input(user, 0);
        guide::input(user, 0);
        signin::input(user, 0);
        store_state(memory, state, &Latch::default());
        ctx.r3.u64 = u64::from(ERROR_DEVICE_NOT_CONNECTED);
        return;
    }

    if pad.is_some() {
        sdl::update();
    }
    let down = |button: Button| pad.is_some_and(|pad| pad.button(button));
    let mut b = BUTTONS.iter().filter(|(button, _)| down(*button)).fold(elsewhere, |b, (_, bit)| b | bit);
    #[cfg(feature = "diagnostics")]
    if let Some(pad) = pad.filter(|_| user == 0) {
        hunt_mark(pad, &mut b);
    }

    let axis = |axis: Axis| pad.map_or(0, |pad| pad.axis(axis));
    let (scripted_triggers, scripted_axes, axis_held) = match script().as_ref() {
        Some(script) => (script.triggers[at], script.axes[at], script.axis_held[at]),
        None => ([0; 2], [0; 4], [false; 4]),
    };
    let mut triggers = [
        (i32::from(axis(Axis::LEFT_TRIGGER)) >> 7).max(i32::from(scripted_triggers[0])) as u8,
        (i32::from(axis(Axis::RIGHT_TRIGGER)) >> 7).max(i32::from(scripted_triggers[1])) as u8,
    ];
    // The guest's Y axes point up; SDL's point down.
    let mut axes = [
        axis(Axis::LEFTX),
        (-1 - i32::from(axis(Axis::LEFTY))) as i16,
        axis(Axis::RIGHTX),
        (-1 - i32::from(axis(Axis::RIGHTY))) as i16,
    ];
    #[cfg(feature = "diagnostics")]
    if user == 0 {
        autopilot(&mut b, &mut triggers, &mut axes);
    }
    // A scripted stick entry overrides the pad and the autopilot while it is held: a
    // script that says where to look means it.
    for i in 0..4 {
        if axis_held[i] {
            axes[i] = scripted_axes[i];
        }
    }

    // The system's screens take the controllers while one is open, the left stick moving
    // through it as the d-pad does: the friends screen, the guide, then the sign-in
    // screen, each reached only while none before it is open. The Guide button -- or Back
    // and Start together, where the system keeps that button for itself, or Home -- opens
    // the guide at the first controller and the sign-in screen at the others.
    let stick = if axes[1] > 16000 {
        DPAD_UP
    } else if axes[1] < -16000 {
        DPAD_DOWN
    } else {
        0
    };
    let guide_pressed = down(Button::GUIDE) || b & (BACK | START) == BACK | START;
    let held = b | stick | if guide_pressed { signin::GUIDE } else { 0 };
    let picking = friends::input(user, held);
    let guided = guide::input(user, if picking { 0 } else { held }) || picking;
    let taken = signin::input(user, if guided { 0 } else { held }) || guided;
    b &= !signin::GUIDE;
    if taken {
        b = 0;
        triggers = [0; 2];
        axes = [0; 4];
    }

    let mut latches = LATCHES.lock().unwrap_or_else(PoisonError::into_inner);
    let latch = &mut latches[at];
    if latch.buttons != b || latch.triggers != triggers || latch.axes != axes {
        *latch = Latch { packet: latch.packet.wrapping_add(1), buttons: b, triggers, axes };
    }
    let latch = *latch;
    drop(latches);
    store_state(memory, state, &latch);
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

kernel_import!(XamInputGetCapabilities, |ctx, memory| {
    let (user, capabilities) = (ctx.r3.as_u32(), ctx.r5.as_u32());
    if capabilities == 0 {
        ctx.r3.u64 = u64::from(ERROR_DEVICE_NOT_CONNECTED);
        return;
    }
    memory.zero(capabilities, 20);
    let present = pad_for(user).is_some() || (user < 4 && scripted(user as usize)) || (user == 0 && player_wanted());
    if !present {
        ctx.r3.u64 = u64::from(ERROR_DEVICE_NOT_CONNECTED);
        return;
    }
    // XINPUT_CAPABILITIES of what a wired 360 pad reports: every control it has at full
    // range, and both motors.
    memory.store_u8(capabilities, 1); // XINPUT_DEVTYPE_GAMEPAD
    memory.store_u8(capabilities + 1, 1); // XINPUT_DEVSUBTYPE_GAMEPAD
    memory.store_u16(capabilities + 4, 0xF3FF);
    memory.store_u8(capabilities + 6, 0xFF);
    memory.store_u8(capabilities + 7, 0xFF);
    for i in 0..6 {
        memory.store_u16(capabilities + 8 + i * 2, 0xFFFF); // the sticks, then the motors
    }
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

// XamInputSetState(user, flags, vibration): the left motor is the heavy, low frequency
// one, as SDL's first argument is. A 360 motor keeps its speed until the next call, so the
// rumble is given SDL's longest duration rather than none: with none, SDL also stops
// resending it to pads that let a rumble lapse.
kernel_import!(XamInputSetState, |ctx, memory| {
    let (user, vibration) = (ctx.r3.as_u32(), ctx.r5.as_u32());
    if user >= 4 {
        ctx.r3.u64 = u64::from(ERROR_DEVICE_NOT_CONNECTED);
        return;
    }
    if vibration == 0 {
        ctx.r3.u64 = u64::from(ERROR_BAD_ARGUMENTS);
        return;
    }
    if let Some(pad) = pad_for(user) {
        let pads = pads();
        if pads.open[user as usize] == Some(pad) {
            let (left, right) = (memory.u16_at(vibration), memory.u16_at(vibration + 2));
            let rumbled = pad.rumble(left, right, 0xFFFF);
            drop(pads);
            // Said once per pad: the first rumble, or that the pad has none.
            static TOLD: Mutex<[bool; 4]> = Mutex::new([false; 4]);
            let mut told = TOLD.lock().unwrap_or_else(PoisonError::into_inner);
            if (left != 0 || right != 0 || !rumbled) && !told[user as usize] {
                told[user as usize] = true;
                if rumbled {
                    log_info!("input: controller %u rumbles (%04X, %04X)", user, u32::from(left), u32::from(right));
                } else {
                    log_warn!("input: controller %u cannot rumble: %s", user, sdl::error().as_c_str());
                }
            }
            ctx.r3.u64 = u64::from(ERROR_SUCCESS);
            return;
        }
    }
    ctx.r3.u64 = u64::from(ERROR_DEVICE_NOT_CONNECTED);
});
