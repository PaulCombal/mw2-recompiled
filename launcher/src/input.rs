// What the window reads of the keyboard, the mouse and a controller, as Dear ImGui
// 1.91.9b's IO reads them, fed by its SDL3 backend (imgui_impl_sdl3.cpp), so that a
// press does the same: SDL's events become a queue of Dear ImGui's events, the
// same ones filtered out as repeats, and each frame takes from the queue as Dear ImGui
// did with its queue trickled (io.ConfigInputTrickleEventQueue) -- a key or a button
// that changes twice is left for the next frame, so a press and its release in one frame
// are still a press -- and the window asks what it asked: whether a key was pressed this
// frame, or again by being held (at the launcher's pace, KEY_REPEAT_RATE), where the
// pointer is, how far it moved, whether the button went down. The time is the backend's,
// the performance counter's, and the pointer is taken from the whole screen while the
// window has the keyboard, as the backend took it on the systems it did that on.
//
// Dear ImGui is Copyright (c) 2014-2025 Omar Cornut, under the MIT licence, whose text is
// in launcher/NOTICE.md.
#![allow(unsafe_code)]

use crate::draw::{Point, int, point};
use sdl3_sys::everything as sdl;
use std::collections::HashMap;
use std::ffi::CStr;

// io.KeyRepeatDelay, Dear ImGui's own, and the launcher's io.KeyRepeatRate: a held
// direction goes through the entries at a pace each one can be seen and heard at, where
// Dear ImGui's own, 20 a second, is a text cursor's.
const KEY_REPEAT_DELAY: f32 = 0.275;
const KEY_REPEAT_RATE: f32 = 0.10;
// A position no pointer has (-FLT_MAX), and Dear ImGui's test for one that is real.
const NOWHERE: f32 = -f32::MAX;
const MOUSE_INVALID: f32 = -256000.0;
const BUTTONS: usize = 5;
// SDL_gamepad.h's suggestion, the backend's.
const THUMB_DEAD_ZONE: f32 = 8000.0;

/// Dear ImGui's keys, those the backend gives: one for each of its ImGuiKey values, as two
/// SDL keys may be one of them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Key {
    Tab,
    LeftArrow,
    RightArrow,
    UpArrow,
    DownArrow,
    PageUp,
    PageDown,
    Home,
    End,
    Insert,
    Delete,
    Backspace,
    Space,
    Enter,
    Escape,
    LeftCtrl,
    LeftShift,
    LeftAlt,
    LeftSuper,
    RightCtrl,
    RightShift,
    RightAlt,
    RightSuper,
    Menu,
    // A digit, a letter, a function key: by its place.
    Digit(u8),
    Letter(u8),
    F(u8),
    Apostrophe,
    Comma,
    Minus,
    Period,
    Slash,
    Semicolon,
    Equal,
    LeftBracket,
    Backslash,
    RightBracket,
    GraveAccent,
    CapsLock,
    ScrollLock,
    NumLock,
    PrintScreen,
    Pause,
    Keypad(u8),
    KeypadDecimal,
    KeypadDivide,
    KeypadMultiply,
    KeypadSubtract,
    KeypadAdd,
    KeypadEnter,
    KeypadEqual,
    AppBack,
    AppForward,
    Oem102,
    GamepadStart,
    GamepadBack,
    GamepadFaceLeft,
    GamepadFaceRight,
    GamepadFaceUp,
    GamepadFaceDown,
    GamepadDpadLeft,
    GamepadDpadRight,
    GamepadDpadUp,
    GamepadDpadDown,
    GamepadL1,
    GamepadR1,
    GamepadL2,
    GamepadR2,
    GamepadL3,
    GamepadR3,
    GamepadLStickLeft,
    GamepadLStickRight,
    GamepadLStickUp,
    GamepadLStickDown,
    GamepadRStickLeft,
    GamepadRStickRight,
    GamepadRStickUp,
    GamepadRStickDown,
    // The modifiers, as the backend gives them from each key event's.
    ModCtrl,
    ModShift,
    ModAlt,
    ModSuper,
}

impl Key {
    fn gamepad(self) -> bool {
        use Key::*;
        matches!(
            self,
            GamepadStart
                | GamepadBack
                | GamepadFaceLeft
                | GamepadFaceRight
                | GamepadFaceUp
                | GamepadFaceDown
                | GamepadDpadLeft
                | GamepadDpadRight
                | GamepadDpadUp
                | GamepadDpadDown
                | GamepadL1
                | GamepadR1
                | GamepadL2
                | GamepadR2
                | GamepadL3
                | GamepadR3
                | GamepadLStickLeft
                | GamepadLStickRight
                | GamepadLStickUp
                | GamepadLStickDown
                | GamepadRStickLeft
                | GamepadRStickRight
                | GamepadRStickUp
                | GamepadRStickDown
        )
    }
}

// ImGui_ImplSDL3_KeyEventToImGuiKey: the keypad by where its keys are, the rest by what
// they type, and a few by where they are when they type nothing it names.
fn key_of(keycode: sdl::SDL_Keycode, scancode: sdl::SDL_Scancode) -> Option<Key> {
    use Key::*;
    use sdl::*;
    let keypad = [
        (SDL_SCANCODE_KP_0, Keypad(0)),
        (SDL_SCANCODE_KP_1, Keypad(1)),
        (SDL_SCANCODE_KP_2, Keypad(2)),
        (SDL_SCANCODE_KP_3, Keypad(3)),
        (SDL_SCANCODE_KP_4, Keypad(4)),
        (SDL_SCANCODE_KP_5, Keypad(5)),
        (SDL_SCANCODE_KP_6, Keypad(6)),
        (SDL_SCANCODE_KP_7, Keypad(7)),
        (SDL_SCANCODE_KP_8, Keypad(8)),
        (SDL_SCANCODE_KP_9, Keypad(9)),
        (SDL_SCANCODE_KP_PERIOD, KeypadDecimal),
        (SDL_SCANCODE_KP_DIVIDE, KeypadDivide),
        (SDL_SCANCODE_KP_MULTIPLY, KeypadMultiply),
        (SDL_SCANCODE_KP_MINUS, KeypadSubtract),
        (SDL_SCANCODE_KP_PLUS, KeypadAdd),
        (SDL_SCANCODE_KP_ENTER, KeypadEnter),
        (SDL_SCANCODE_KP_EQUALS, KeypadEqual),
    ];
    if let Some(&(_, key)) = keypad.iter().find(|(code, _)| *code == scancode) {
        return Some(key);
    }
    let named = [
        (SDLK_TAB, Tab),
        (SDLK_LEFT, LeftArrow),
        (SDLK_RIGHT, RightArrow),
        (SDLK_UP, UpArrow),
        (SDLK_DOWN, DownArrow),
        (SDLK_PAGEUP, PageUp),
        (SDLK_PAGEDOWN, PageDown),
        (SDLK_HOME, Home),
        (SDLK_END, End),
        (SDLK_INSERT, Insert),
        (SDLK_DELETE, Delete),
        (SDLK_BACKSPACE, Backspace),
        (SDLK_SPACE, Space),
        (SDLK_RETURN, Enter),
        (SDLK_ESCAPE, Escape),
        (SDLK_COMMA, Comma),
        (SDLK_PERIOD, Period),
        (SDLK_SEMICOLON, Semicolon),
        (SDLK_CAPSLOCK, CapsLock),
        (SDLK_SCROLLLOCK, ScrollLock),
        (SDLK_NUMLOCKCLEAR, NumLock),
        (SDLK_PRINTSCREEN, PrintScreen),
        (SDLK_PAUSE, Pause),
        (SDLK_LCTRL, LeftCtrl),
        (SDLK_LSHIFT, LeftShift),
        (SDLK_LALT, LeftAlt),
        (SDLK_LGUI, LeftSuper),
        (SDLK_RCTRL, RightCtrl),
        (SDLK_RSHIFT, RightShift),
        (SDLK_RALT, RightAlt),
        (SDLK_RGUI, RightSuper),
        (SDLK_APPLICATION, Menu),
        (SDLK_AC_BACK, AppBack),
        (SDLK_AC_FORWARD, AppForward),
    ];
    if let Some(&(_, key)) = named.iter().find(|(code, _)| *code == keycode) {
        return Some(key);
    }
    let code = keycode.0;
    if (SDLK_0.0..=SDLK_9.0).contains(&code) {
        return Some(Digit((code - SDLK_0.0) as u8));
    }
    if (SDLK_A.0..=SDLK_Z.0).contains(&code) {
        return Some(Letter((code - SDLK_A.0) as u8));
    }
    if (SDLK_F1.0..=SDLK_F12.0).contains(&code) {
        return Some(F((code - SDLK_F1.0) as u8 + 1));
    }
    if (SDLK_F13.0..=SDLK_F24.0).contains(&code) {
        return Some(F((code - SDLK_F13.0) as u8 + 13));
    }
    let placed = [
        (SDL_SCANCODE_GRAVE, GraveAccent),
        (SDL_SCANCODE_MINUS, Minus),
        (SDL_SCANCODE_EQUALS, Equal),
        (SDL_SCANCODE_LEFTBRACKET, LeftBracket),
        (SDL_SCANCODE_RIGHTBRACKET, RightBracket),
        (SDL_SCANCODE_NONUSBACKSLASH, Oem102),
        (SDL_SCANCODE_BACKSLASH, Backslash),
        (SDL_SCANCODE_SEMICOLON, Semicolon),
        (SDL_SCANCODE_APOSTROPHE, Apostrophe),
        (SDL_SCANCODE_COMMA, Comma),
        (SDL_SCANCODE_PERIOD, Period),
        (SDL_SCANCODE_SLASH, Slash),
    ];
    placed.iter().find(|(code, _)| *code == scancode).map(|&(_, key)| key)
}

#[derive(Clone, Copy)]
struct KeyData {
    down: bool,
    // How long it has been down, 0 the frame it went down, below 0 while it is up.
    duration: f32,
    analog: f32,
}

const UP: KeyData = KeyData { down: false, duration: -1.0, analog: 0.0 };

// Dear ImGui's ImGuiInputEvent, as much of it as decides anything here.
#[derive(Clone, Copy)]
enum Event {
    MousePos { x: f32, y: f32 },
    MouseButton { button: usize, down: bool, touch: bool },
    MouseWheel,
    Key { key: Key, down: bool, analog: f32 },
    Text,
    Focus(bool),
}

pub struct Input {
    window: *mut sdl::SDL_Window,
    window_id: sdl::SDL_WindowID,
    queue: Vec<Event>,
    keys: HashMap<Key, KeyData>,
    pos: Point,
    pos_prev: Point,
    delta: Point,
    down: [bool; BUTTONS],
    down_duration: [f32; BUTTONS],
    clicked_pos: [Point; BUTTONS],
    drag_distance_sqr: [f32; BUTTONS],
    focus_lost: bool,
    // The mouse source the next mouse event is stamped with: a touch SDL made a mouse of.
    touch: bool,
    time: f64,
    delta_time: f32,
    frame: i32,
    // The backend's.
    counter: u64,
    frequency: u64,
    buttons_down: u32,
    pending_leave: i32,
    global_mouse: bool,
    cursor: *mut sdl::SDL_Cursor,
    gamepads: Vec<*mut sdl::SDL_Gamepad>,
    update_gamepads: bool,
    has_gamepad: bool,
    size: Point,
    scale: Point,
}

impl Input {
    /// For `window`, as ImGui_ImplSDL3_Init set it up.
    pub fn new(window: *mut sdl::SDL_Window) -> Input {
        // SAFETY: the caller's window, alive as long as this; the hints' names and values
        // are C strings.
        let (window_id, global_mouse, cursor, frequency) = unsafe {
            let driver = sdl::SDL_GetCurrentVideoDriver();
            let driver = if driver.is_null() { &[][..] } else { CStr::from_ptr(driver).to_bytes() };
            let global =
                [&b"windows"[..], b"cocoa", b"x11", b"DIVE", b"VMAN"].iter().any(|name| driver.starts_with(name));
            sdl::SDL_SetHint(sdl::SDL_HINT_MOUSE_FOCUS_CLICKTHROUGH, c"1".as_ptr());
            sdl::SDL_SetHint(sdl::SDL_HINT_MOUSE_AUTO_CAPTURE, c"0".as_ptr());
            (
                sdl::SDL_GetWindowID(window),
                global,
                sdl::SDL_CreateSystemCursor(sdl::SDL_SYSTEM_CURSOR_DEFAULT),
                sdl::SDL_GetPerformanceFrequency(),
            )
        };
        Input {
            window,
            window_id,
            queue: Vec::new(),
            keys: HashMap::new(),
            pos: point(NOWHERE, NOWHERE),
            pos_prev: point(NOWHERE, NOWHERE),
            delta: Point::default(),
            down: [false; BUTTONS],
            down_duration: [-1.0; BUTTONS],
            clicked_pos: [Point::default(); BUTTONS],
            drag_distance_sqr: [0.0; BUTTONS],
            focus_lost: false,
            touch: false,
            time: 0.0,
            delta_time: 1.0 / 60.0,
            frame: 0,
            counter: 0,
            frequency,
            buttons_down: 0,
            pending_leave: 0,
            global_mouse,
            cursor,
            gamepads: Vec::new(),
            update_gamepads: true,
            has_gamepad: false,
            size: Point::default(),
            scale: point(1.0, 1.0),
        }
    }

    fn key(&self, key: Key) -> KeyData {
        self.keys.get(&key).copied().unwrap_or(UP)
    }

    // ---- io.Add*Event: what a frame takes from its queue -----------------------------

    fn add_mouse_pos(&mut self, x: f32, y: f32) {
        let floor = |v: f32| {
            if v > NOWHERE {
                let i = int(v);
                (if v >= 0.0 || i as f32 == v { i } else { i.wrapping_sub(1) }) as f32
            } else {
                v
            }
        };
        let (x, y) = (floor(x), floor(y));
        let latest = self.queue.iter().rev().find_map(|e| match *e {
            Event::MousePos { x, y } => Some(point(x, y)),
            _ => None,
        });
        let latest = latest.unwrap_or(self.pos);
        if latest.x == x && latest.y == y {
            return;
        }
        self.queue.push(Event::MousePos { x, y });
    }

    fn add_mouse_button(&mut self, button: usize, down: bool) {
        let latest = self.queue.iter().rev().find_map(|e| match *e {
            Event::MouseButton { button: b, down, .. } if b == button => Some(down),
            _ => None,
        });
        if latest.unwrap_or(self.down[button]) == down {
            return;
        }
        self.queue.push(Event::MouseButton { button, down, touch: self.touch });
    }

    fn add_key(&mut self, key: Key, down: bool, analog: f32) {
        let latest = self.queue.iter().rev().find_map(|e| match *e {
            Event::Key { key: k, down, analog } if k == key => Some((down, analog)),
            _ => None,
        });
        let data = self.key(key);
        let (latest_down, latest_analog) = latest.unwrap_or((data.down, data.analog));
        if latest_down == down && latest_analog == analog {
            return;
        }
        self.queue.push(Event::Key { key, down, analog });
    }

    fn add_focus(&mut self, focused: bool) {
        let latest = self.queue.iter().rev().find_map(|e| match *e {
            Event::Focus(focused) => Some(focused),
            _ => None,
        });
        if latest.unwrap_or(!self.focus_lost) == focused {
            return;
        }
        self.queue.push(Event::Focus(focused));
    }

    /// ImGui_ImplSDL3_ProcessEvent.
    pub fn event(&mut self, event: &sdl::SDL_Event) {
        // SAFETY: the union's field that its type names.
        unsafe {
            let kind = sdl::SDL_EventType(event.r#type);
            let ours = |id: sdl::SDL_WindowID| id == self.window_id;
            if kind == sdl::SDL_EVENT_MOUSE_MOTION && ours(event.motion.windowID) {
                self.touch = event.motion.which == sdl::SDL_TOUCH_MOUSEID;
                self.add_mouse_pos(event.motion.x, event.motion.y);
            } else if kind == sdl::SDL_EVENT_MOUSE_WHEEL && ours(event.wheel.windowID) {
                self.touch = event.wheel.which == sdl::SDL_TOUCH_MOUSEID;
                if -event.wheel.x != 0.0 || event.wheel.y != 0.0 {
                    self.queue.push(Event::MouseWheel);
                }
            } else if (kind == sdl::SDL_EVENT_MOUSE_BUTTON_DOWN || kind == sdl::SDL_EVENT_MOUSE_BUTTON_UP)
                && ours(event.button.windowID)
            {
                let button = match i32::from(event.button.button) {
                    sdl::SDL_BUTTON_LEFT => 0,
                    sdl::SDL_BUTTON_RIGHT => 1,
                    sdl::SDL_BUTTON_MIDDLE => 2,
                    sdl::SDL_BUTTON_X1 => 3,
                    sdl::SDL_BUTTON_X2 => 4,
                    _ => return,
                };
                self.touch = event.button.which == sdl::SDL_TOUCH_MOUSEID;
                let down = kind == sdl::SDL_EVENT_MOUSE_BUTTON_DOWN;
                self.add_mouse_button(button, down);
                if down {
                    self.buttons_down |= 1 << button;
                } else {
                    self.buttons_down &= !(1 << button);
                }
            } else if kind == sdl::SDL_EVENT_TEXT_INPUT && ours(event.text.windowID) {
                // A character each, nothing but the order of the queue hangs on them.
                if !event.text.text.is_null() && *event.text.text != 0 {
                    self.queue.push(Event::Text);
                }
            } else if (kind == sdl::SDL_EVENT_KEY_DOWN || kind == sdl::SDL_EVENT_KEY_UP) && ours(event.key.windowID) {
                let modifiers = event.key.r#mod.0;
                for (key, mask) in [
                    (Key::ModCtrl, sdl::SDL_KMOD_CTRL),
                    (Key::ModShift, sdl::SDL_KMOD_SHIFT),
                    (Key::ModAlt, sdl::SDL_KMOD_ALT),
                    (Key::ModSuper, sdl::SDL_KMOD_GUI),
                ] {
                    let on = modifiers & mask.0 != 0;
                    self.add_key(key, on, if on { 1.0 } else { 0.0 });
                }
                if let Some(key) = key_of(event.key.key, event.key.scancode) {
                    let down = kind == sdl::SDL_EVENT_KEY_DOWN;
                    self.add_key(key, down, if down { 1.0 } else { 0.0 });
                }
            } else if kind == sdl::SDL_EVENT_WINDOW_MOUSE_ENTER && ours(event.window.windowID) {
                self.pending_leave = 0;
            } else if kind == sdl::SDL_EVENT_WINDOW_MOUSE_LEAVE && ours(event.window.windowID) {
                self.pending_leave = self.frame + 1;
            } else if (kind == sdl::SDL_EVENT_WINDOW_FOCUS_GAINED || kind == sdl::SDL_EVENT_WINDOW_FOCUS_LOST)
                && ours(event.window.windowID)
            {
                self.add_focus(kind == sdl::SDL_EVENT_WINDOW_FOCUS_GAINED);
            } else if kind == sdl::SDL_EVENT_GAMEPAD_ADDED || kind == sdl::SDL_EVENT_GAMEPAD_REMOVED {
                self.update_gamepads = true;
            }
        }
    }

    // ---- a frame ---------------------------------------------------------------------

    /// ImGui_ImplSDL3_NewFrame, then what ImGui::NewFrame does with the input.
    pub fn new_frame(&mut self) {
        // SAFETY: SDL's clock.
        self.new_frame_at(unsafe { sdl::SDL_GetPerformanceCounter() });
    }

    /// The same, the performance counter at `now`.
    pub fn new_frame_at(&mut self, now: u64) {
        // SAFETY: the window this was made for, alive; SDL's own calls on it.
        unsafe {
            let (mut w, mut h, mut display_w, mut display_h) = (0, 0, 0, 0);
            sdl::SDL_GetWindowSize(self.window, &mut w, &mut h);
            if sdl::SDL_GetWindowFlags(self.window).0 & sdl::SDL_WINDOW_MINIMIZED.0 != 0 {
                (w, h) = (0, 0);
            }
            sdl::SDL_GetWindowSizeInPixels(self.window, &mut display_w, &mut display_h);
            self.size = point(w as f32, h as f32);
            if w > 0 && h > 0 {
                self.scale = point(display_w as f32 / w as f32, display_h as f32 / h as f32);
            }

            let mut now = now;
            if now <= self.counter {
                now = self.counter + 1;
            }
            self.delta_time = if self.counter > 0 {
                ((now - self.counter) as f64 / self.frequency as f64) as f32
            } else {
                1.0 / 60.0
            };
            self.counter = now;

            if self.pending_leave != 0 && self.pending_leave >= self.frame && self.buttons_down == 0 {
                self.pending_leave = 0;
                self.add_mouse_pos(NOWHERE, NOWHERE);
            }

            // The pointer held past the window's edge, while it drags, is still followed.
            let dragging = (0..BUTTONS).any(|b| self.down[b] && self.drag_distance_sqr[b] >= 1.0);
            sdl::SDL_CaptureMouse(dragging);
            let focused = sdl::SDL_GetKeyboardFocus();
            if focused == self.window
                && self.global_mouse
                && self.buttons_down == 0
                && !sdl::SDL_GetWindowRelativeMouseMode(self.window)
            {
                let (mut x, mut y) = (0.0f32, 0.0f32);
                let (mut wx, mut wy) = (0, 0);
                sdl::SDL_GetGlobalMouseState(&mut x, &mut y);
                sdl::SDL_GetWindowPosition(focused, &mut wx, &mut wy);
                self.add_mouse_pos(x - wx as f32, y - wy as f32);
            }

            if !self.cursor.is_null() {
                if self.frame == 0 {
                    sdl::SDL_SetCursor(self.cursor);
                }
                sdl::SDL_ShowCursor();
            }
            self.gamepads();
        }

        self.time += f64::from(self.delta_time);
        self.frame += 1;
        self.take_events();
        self.update_keys();
        self.update_mouse();
    }

    // ImGui_ImplSDL3_UpdateGamepads: the first controller there is, opened again when one
    // comes or goes, its buttons and sticks as keys.
    #[allow(clippy::type_complexity)]
    unsafe fn gamepads(&mut self) {
        unsafe {
            if self.update_gamepads {
                for &gamepad in &self.gamepads {
                    sdl::SDL_CloseGamepad(gamepad);
                }
                self.gamepads.clear();
                let mut count = 0;
                let ids = sdl::SDL_GetGamepads(&mut count);
                for n in 0..usize::try_from(count).unwrap_or(0) {
                    let gamepad = sdl::SDL_OpenGamepad(*ids.add(n));
                    if !gamepad.is_null() {
                        self.gamepads.push(gamepad);
                        break;
                    }
                }
                self.update_gamepads = false;
                sdl::SDL_free(ids.cast());
            }
            self.has_gamepad = !self.gamepads.is_empty();
            if !self.has_gamepad {
                return;
            }
            use sdl::*;
            let buttons: [(Key, SDL_GamepadButton); 14] = [
                (Key::GamepadStart, SDL_GAMEPAD_BUTTON_START),
                (Key::GamepadBack, SDL_GAMEPAD_BUTTON_BACK),
                (Key::GamepadFaceLeft, SDL_GAMEPAD_BUTTON_WEST),
                (Key::GamepadFaceRight, SDL_GAMEPAD_BUTTON_EAST),
                (Key::GamepadFaceUp, SDL_GAMEPAD_BUTTON_NORTH),
                (Key::GamepadFaceDown, SDL_GAMEPAD_BUTTON_SOUTH),
                (Key::GamepadDpadLeft, SDL_GAMEPAD_BUTTON_DPAD_LEFT),
                (Key::GamepadDpadRight, SDL_GAMEPAD_BUTTON_DPAD_RIGHT),
                (Key::GamepadDpadUp, SDL_GAMEPAD_BUTTON_DPAD_UP),
                (Key::GamepadDpadDown, SDL_GAMEPAD_BUTTON_DPAD_DOWN),
                (Key::GamepadL1, SDL_GAMEPAD_BUTTON_LEFT_SHOULDER),
                (Key::GamepadR1, SDL_GAMEPAD_BUTTON_RIGHT_SHOULDER),
                (Key::GamepadL3, SDL_GAMEPAD_BUTTON_LEFT_STICK),
                (Key::GamepadR3, SDL_GAMEPAD_BUTTON_RIGHT_STICK),
            ];
            let button = |this: &mut Input, key: Key, which: SDL_GamepadButton| {
                let down = this.gamepads.iter().any(|&g| SDL_GetGamepadButton(g, which));
                this.add_key(key, down, if down { 1.0 } else { 0.0 });
            };
            let analog = |this: &mut Input, key: Key, axis: SDL_GamepadAxis, v0: f32, v1: f32| {
                let mut merged = 0.0f32;
                for &g in &this.gamepads {
                    let v = (f32::from(SDL_GetGamepadAxis(g, axis)) - v0) / (v1 - v0);
                    // Saturate, which keeps a NaN as clamp does.
                    let v = v.clamp(0.0, 1.0);
                    if merged < v {
                        merged = v;
                    }
                }
                this.add_key(key, merged > 0.1, merged);
            };
            // In the backend's order, which is the queue's.
            for &(key, which) in &buttons[..12] {
                button(self, key, which);
            }
            analog(self, Key::GamepadL2, SDL_GAMEPAD_AXIS_LEFT_TRIGGER, 0.0, 32767.0);
            analog(self, Key::GamepadR2, SDL_GAMEPAD_AXIS_RIGHT_TRIGGER, 0.0, 32767.0);
            for &(key, which) in &buttons[12..] {
                button(self, key, which);
            }
            let dead = THUMB_DEAD_ZONE;
            analog(self, Key::GamepadLStickLeft, SDL_GAMEPAD_AXIS_LEFTX, -dead, -32768.0);
            analog(self, Key::GamepadLStickRight, SDL_GAMEPAD_AXIS_LEFTX, dead, 32767.0);
            analog(self, Key::GamepadLStickUp, SDL_GAMEPAD_AXIS_LEFTY, -dead, -32768.0);
            analog(self, Key::GamepadLStickDown, SDL_GAMEPAD_AXIS_LEFTY, dead, 32767.0);
            analog(self, Key::GamepadRStickLeft, SDL_GAMEPAD_AXIS_RIGHTX, -dead, -32768.0);
            analog(self, Key::GamepadRStickRight, SDL_GAMEPAD_AXIS_RIGHTX, dead, 32767.0);
            analog(self, Key::GamepadRStickUp, SDL_GAMEPAD_AXIS_RIGHTY, -dead, -32768.0);
            analog(self, Key::GamepadRStickDown, SDL_GAMEPAD_AXIS_RIGHTY, dead, 32767.0);
        }
    }

    // ImGui::UpdateInputEvents, trickled: the queue up to the first event that would make a
    // key or a button change twice in the frame, or move the pointer after a press.
    fn take_events(&mut self) {
        let (mut moved, mut wheeled, mut key_changed) = (false, false, false);
        let mut buttons_changed = 0u32;
        let mut keys_changed: Vec<Key> = Vec::new();
        let mut taken = 0;
        for &event in &self.queue {
            match event {
                Event::MousePos { x, y } => {
                    if buttons_changed != 0 || wheeled || key_changed {
                        break;
                    }
                    self.pos = point(x, y);
                    moved = true;
                }
                Event::MouseButton { button, down, touch } => {
                    if buttons_changed & (1 << button) != 0 || wheeled {
                        break;
                    }
                    if touch && moved {
                        break;
                    }
                    self.down[button] = down;
                    buttons_changed |= 1 << button;
                }
                Event::MouseWheel => {
                    if moved || buttons_changed != 0 {
                        break;
                    }
                    wheeled = true;
                }
                Event::Key { key, down, analog } => {
                    let data = self.keys.entry(key).or_insert(UP);
                    if data.down != down && (keys_changed.contains(&key) || buttons_changed != 0) {
                        break;
                    }
                    data.down = down;
                    data.analog = analog;
                    key_changed = true;
                    if !keys_changed.contains(&key) {
                        keys_changed.push(key);
                    }
                }
                Event::Text => {
                    if buttons_changed != 0 || moved || wheeled {
                        break;
                    }
                }
                Event::Focus(focused) => self.focus_lost = !focused,
            }
            taken += 1;
        }
        self.queue.drain(..taken);
        // ClearInputKeys and ClearInputMouse while the window has lost the focus.
        if self.focus_lost {
            for data in self.keys.values_mut() {
                data.down = false;
                data.duration = -1.0;
            }
            self.pos = point(NOWHERE, NOWHERE);
            self.down = [false; BUTTONS];
            self.down_duration = [-1.0; BUTTONS];
        }
    }

    // ImGui::UpdateKeyboardInputs: a controller's keys up while there is none, and how long
    // each key has been down.
    fn update_keys(&mut self) {
        let dt = self.delta_time;
        for (key, data) in self.keys.iter_mut() {
            if !self.has_gamepad && key.gamepad() {
                data.down = false;
                data.analog = 0.0;
            }
            data.duration = if data.down { if data.duration < 0.0 { 0.0 } else { data.duration + dt } } else { -1.0 };
        }
    }

    // ImGui::UpdateMouseInputs: how far the pointer moved, and the button's state.
    fn update_mouse(&mut self) {
        let valid = |p: Point| p.x >= MOUSE_INVALID && p.y >= MOUSE_INVALID;
        self.delta = if valid(self.pos) && valid(self.pos_prev) {
            point(self.pos.x - self.pos_prev.x, self.pos.y - self.pos_prev.y)
        } else {
            Point::default()
        };
        for b in 0..BUTTONS {
            let clicked = self.down[b] && self.down_duration[b] < 0.0;
            self.down_duration[b] = if self.down[b] {
                if self.down_duration[b] < 0.0 { 0.0 } else { self.down_duration[b] + self.delta_time }
            } else {
                -1.0
            };
            if clicked {
                self.clicked_pos[b] = self.pos;
                self.drag_distance_sqr[b] = 0.0;
            } else if self.down[b] {
                let d = if valid(self.pos) {
                    let (dx, dy) = (self.pos.x - self.clicked_pos[b].x, self.pos.y - self.clicked_pos[b].y);
                    dx * dx + dy * dy
                } else {
                    0.0
                };
                self.drag_distance_sqr[b] = crate::draw::im_max(self.drag_distance_sqr[b], d);
            }
        }
    }

    /// ImGui::EndFrame's part: where the pointer was, for the next frame's movement.
    pub fn end_frame(&mut self) {
        self.pos_prev = self.pos;
        self.focus_lost = false;
    }

    // ---- what the window asks ----------------------------------------------------------

    /// ImGui::IsKeyPressed of any of `keys`: down this frame, or with `repeat` again by
    /// being held.
    pub fn pressed(&self, keys: &[Key], repeat: bool) -> bool {
        keys.iter().any(|&key| {
            let data = self.key(key);
            let t = data.duration;
            if !data.down || t < 0.0 {
                return false;
            }
            t == 0.0 || (repeat && t > KEY_REPEAT_DELAY && repeat_amount(t - self.delta_time, t) > 0)
        })
    }

    /// ImGui::IsMouseClicked(left): the button went down this frame.
    pub fn clicked(&self) -> bool {
        self.down[0] && self.down_duration[0] == 0.0
    }

    pub fn mouse_down(&self) -> bool {
        self.down[0]
    }

    pub fn mouse_pos(&self) -> Point {
        self.pos
    }

    pub fn mouse_delta(&self) -> Point {
        self.delta
    }

    /// ImGui::GetTime: the frames' times, summed.
    pub fn time(&self) -> f64 {
        self.time
    }

    pub fn delta_time(&self) -> f32 {
        self.delta_time
    }

    /// The window's size in its own units, none while minimized, and how many pixels a
    /// unit has.
    pub fn display(&self) -> (Point, Point) {
        (self.size, self.scale)
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        // SAFETY: what new() and gamepads() opened.
        unsafe {
            for &gamepad in &self.gamepads {
                sdl::SDL_CloseGamepad(gamepad);
            }
            if !self.cursor.is_null() {
                sdl::SDL_DestroyCursor(self.cursor);
            }
        }
    }
}

// ImGui::CalcTypematicRepeatAmount at the launcher's pace: how many repeats fall between
// having been held t0 and t1 seconds.
fn repeat_amount(t0: f32, t1: f32) -> i32 {
    if t1 == 0.0 {
        return 1;
    }
    if t0 >= t1 {
        return 0;
    }
    let count = |t: f32| if t < KEY_REPEAT_DELAY { -1 } else { int((t - KEY_REPEAT_DELAY) / KEY_REPEAT_RATE) };
    count(t1) - count(t0)
}
