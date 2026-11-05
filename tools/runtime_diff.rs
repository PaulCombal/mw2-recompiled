// What tools/runtime_diff.cpp calls of the Rust runtime that nothing else exports.

use crate::runtime::install::{crypto, xex};
use crate::runtime::render_size::{self, Size};
use std::ffi::{CStr, c_char};
use std::slice;

#[unsafe(no_mangle)]
unsafe extern "C" fn rig_sha256(data: *const u8, size: usize, out: *mut c_char) {
    let hex = crypto::sha256_hex(if size == 0 { &[] } else { unsafe { slice::from_raw_parts(data, size) } });
    unsafe { std::ptr::copy_nonoverlapping(hex.as_ptr().cast(), out, 64) };
}

#[unsafe(no_mangle)]
unsafe extern "C" fn rig_aes(key: *const [u8; 16], data: *mut u8, size: usize) {
    crypto::aes128_cbc_decrypt(unsafe { &*key }, unsafe { slice::from_raw_parts_mut(data, size) });
}

#[unsafe(no_mangle)]
unsafe extern "C" fn rig_xex(file: *const u8, size: usize, to: *mut u8, room: usize, error: *mut c_char, written: *mut usize) -> bool {
    match xex::image(unsafe { slice::from_raw_parts(file, size) }) {
        Ok(image) => {
            assert!(image.len() <= room);
            unsafe {
                std::ptr::copy_nonoverlapping(image.as_ptr(), to, image.len());
                *written = image.len();
            }
            true
        }
        Err(text) => {
            unsafe {
                std::ptr::copy_nonoverlapping(text.as_ptr().cast(), error, text.len());
                *error.add(text.len()) = 0;
            }
            false
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn rig_parse(text: *const c_char, size: *mut Size) -> bool {
    let Some(parsed) = render_size::parse(unsafe { CStr::from_ptr(text) }.to_bytes()) else { return false };
    unsafe { size.write(parsed) };
    true
}

#[unsafe(no_mangle)]
extern "C" fn rig_scene(frame: Size) -> Size {
    render_size::scene(frame)
}

#[unsafe(no_mangle)]
extern "C" fn rig_frame_valid(frame: Size) -> bool {
    render_size::frame_valid(frame)
}

// What the modules with a clock are given: the rig's time and machine.
mod clocked {
    use crate::runtime::bridge::WatchCosts;
    use crate::runtime::pacing_trace;
    use crate::runtime::platform::{Load, MemoryState};
    use crate::runtime::player::{self, Press, Where};
    use crate::runtime::report::{self, Machine};
    use crate::runtime::stutters;
    use std::ffi::{CStr, c_char, c_int};

    unsafe extern "C" {
        static mut rig_nanoseconds: u64;
        fn rig_load(load: *mut Load) -> bool;
        fn rig_memory(memory: *mut MemoryState);
        fn rig_watch(costs: *mut WatchCosts);
        fn rig_thread_clock() -> usize;
        fn rig_thread_time(clock: usize, nanoseconds: *mut u64) -> bool;
    }

    struct Scripted;

    impl Machine for Scripted {
        fn load(&self, load: &mut Load) -> bool {
            unsafe { rig_load(load) }
        }

        fn memory(&self) -> MemoryState {
            let mut memory = MemoryState::default();
            unsafe { rig_memory(&mut memory) };
            memory
        }

        fn watch(&self) -> WatchCosts {
            let mut costs = WatchCosts::default();
            unsafe { rig_watch(&mut costs) };
            costs
        }

        fn thread_clock(&self) -> usize {
            unsafe { rig_thread_clock() }
        }

        fn thread_time(&self, clock: usize) -> Option<u64> {
            let mut time = 0;
            unsafe { rig_thread_time(clock, &mut time) }.then_some(time)
        }

        fn processor_threads(&self) -> u32 {
            crate::runtime::platform::processor_threads()
        }
    }

    fn now() -> u64 {
        unsafe { rig_nanoseconds }
    }

    fn now_us() -> i64 {
        pacing_trace::microseconds(now())
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_report_frame(world: bool) {
        report::frame(now(), &Scripted, world);
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_report_add(cost: c_int, microseconds: u64) {
        report::add(cost as usize, microseconds);
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_report_spent(time: c_int, nanoseconds: u64) {
        report::spent(time as usize, nanoseconds);
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_report_shader_built(from_cache: c_int) {
        report::shader_built(from_cache);
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn rig_report_thread_started(name: *const c_char) {
        report::thread_started(&Scripted, unsafe { CStr::from_ptr(name) });
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_report_thread_ended() {
        report::thread_ended(&Scripted);
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn rig_report_shaders_compiled(state: *const c_char) {
        report::shaders_compiled(unsafe { CStr::from_ptr(state) }.to_bytes());
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_report_timed_begin(time: c_int) -> u64 {
        report::timed_begin(now(), time as usize)
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_report_timed_end(time: c_int, began: u64) {
        report::timed_end(now(), time as usize, began);
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_report_write() {
        report::write(&Scripted);
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_stutters_wanted() -> bool {
        stutters::wanted()
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_stutters_add(cost: c_int, nanoseconds: u64) {
        stutters::add(cost as usize, nanoseconds);
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_stutters_title_presented() {
        stutters::title_presented(now_us());
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_stutters_finished(world: bool) {
        stutters::finished(now_us(), world, || ());
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_stutters_shown(serial: u64) {
        stutters::shown(now_us(), serial);
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_stutters_displayed(serial: u64, extra_blanks: u32, gap_ms: f64) {
        stutters::displayed(now_us(), serial, extra_blanks, gap_ms);
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn rig_stutters_mark(how: *const c_char) {
        stutters::mark(now_us(), unsafe { CStr::from_ptr(how) });
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_stutters_report() {
        stutters::report();
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_pacing_note(event: u8, value: u32) {
        pacing_trace::note(now(), event, value, crate::runtime::platform::thread_id());
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_pacing_note_callers(r1: u32) {
        pacing_trace::note_callers(|caller| rig_pacing_note(pacing_trace::CALLER, caller), r1);
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_pacing_write() {
        if let Some(path) = pacing_trace::path() {
            pacing_trace::write(path);
        }
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_player_wanted() -> bool {
        player::wanted()
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_player_walking() -> bool {
        player::walking()
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_player_arrived() -> bool {
        player::arrived()
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_player_seconds() -> f64 {
        player::seconds_since_progress(now())
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn rig_player_locate(out: *mut Where) -> bool {
        let Some(here) = player::locate() else { return false };
        unsafe { out.write(here) };
        true
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn rig_player_steer(axes: *mut [i16; 4], pressed: *mut c_int) -> bool {
        let mut press = Press::None;
        let wrote = player::steer(now(), unsafe { &mut *axes }, &mut press);
        unsafe { pressed.write(press as c_int) };
        wrote
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_player_saw_pad(buttons: u16, right_trigger: u8) {
        player::saw_pad(buttons, right_trigger);
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_player_report() {
        player::report();
    }
}

// The online service's pure parts.
mod online {
    use crate::runtime::online::steam::{elsewhere, game_text, read_game, read_session};
    use crate::runtime::online::{SessionFound, SessionListing, SessionNumber, is_connect_line};
    use std::collections::BTreeMap;
    use std::ffi::{CStr, c_char, c_void};
    use std::slice;

    type TextSink = unsafe extern "C" fn(to: *mut c_void, text: *const c_char, size: usize);

    #[unsafe(no_mangle)]
    unsafe extern "C" fn rig_is_connect_line(line: *const u8, size: usize) -> bool {
        is_connect_line(if size == 0 { &[] } else { unsafe { slice::from_raw_parts(line, size) } })
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn rig_read_game(
        text: *const c_char,
        counts: *mut u64,
        session: *mut u8,
        numbers: *mut u32,
        count: *mut usize,
    ) -> bool {
        let mut found = SessionFound::default();
        let mut read = BTreeMap::new();
        let ok = read_game(Some(unsafe { CStr::from_ptr(text) }.to_bytes()), &mut found, &mut read);
        unsafe {
            let counts = slice::from_raw_parts_mut(counts, 4);
            counts.copy_from_slice(&[
                found.open_public.into(),
                found.open_private.into(),
                found.filled_public.into(),
                found.filled_private.into(),
            ]);
            slice::from_raw_parts_mut(session, 60).copy_from_slice(&found.session);
            *count = read.len();
            for (i, (id, value)) in read.into_iter().enumerate() {
                *numbers.add(2 * i) = id;
                *numbers.add(2 * i + 1) = value;
            }
        }
        ok
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn rig_read_session(hex: *const c_char, session: *mut u8) -> bool {
        let session = unsafe { &mut *session.cast::<[u8; 60]>() };
        read_session(Some(unsafe { CStr::from_ptr(hex) }.to_bytes()), session)
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn rig_elsewhere(playing: *const c_char, out: *mut c_char, room: usize) {
        let text = elsewhere(unsafe { CStr::from_ptr(playing) }.to_bytes());
        let length = text.len().min(room - 1);
        unsafe {
            std::ptr::copy_nonoverlapping(text.as_ptr().cast(), out, length);
            *out.add(length) = 0;
        }
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn rig_game_text(
        session: *const u8,
        counts: *const u32,
        numbers: *const u32,
        count: usize,
        text: *mut c_void,
        sink: TextSink,
    ) {
        let counts = unsafe { slice::from_raw_parts(counts, 4) };
        let numbers = if count == 0 { &[][..] } else { unsafe { slice::from_raw_parts(numbers, 2 * count) } };
        let listing = SessionListing {
            session: unsafe { *session.cast::<[u8; 60]>() },
            open_public: counts[0],
            open_private: counts[1],
            filled_public: counts[2],
            filled_private: counts[3],
            numbers: numbers.chunks_exact(2).map(|pair| SessionNumber { id: pair[0], value: pair[1] }).collect(),
        };
        let game = game_text(&listing);
        unsafe { sink(text, game.as_ptr().cast(), game.len()) };
    }
}

// The rig's online service, standing in for Steam: the friend list the C++ rig keeps
// (runtime_diff.cpp's RigBackend reads the same).
mod service {
    use crate::log::log_info;
    use crate::runtime::online::{self, Datagram, Friend, FriendList, Service, Started};
    use std::ffi::{c_char, c_void};

    type TextSink = unsafe extern "C" fn(to: *mut c_void, text: *const c_char, size: usize);

    unsafe extern "C" {
        fn rig_friends_version() -> u64;
        fn rig_friends_counts(playing: *mut usize, online: *mut i32, offline: *mut i32);
        fn rig_friend(
            at: usize,
            id: *mut u64,
            name: *mut c_void,
            sink: TextSink,
            with_you: *mut bool,
            elsewhere: *mut i32,
            avatar: *mut c_void,
            side: *mut u32,
        );
        fn rig_invite_friend(id: u64) -> bool;
        fn rig_invite_friends();
        fn rig_local_name(name: *mut c_void, sink: TextSink);
    }

    unsafe extern "C" fn append(to: *mut c_void, text: *const c_char, size: usize) {
        // An empty string's pointer may be null.
        if size == 0 {
            return;
        }
        let to = unsafe { &mut *to.cast::<Vec<u8>>() };
        to.extend_from_slice(unsafe { std::slice::from_raw_parts(text.cast::<u8>(), size) });
    }

    const ELSEWHERE: [&str; 4] = ["", "On another release", "In multiplayer", "In the campaign or Spec Ops"];

    struct Rig {
        friends: bool,
    }

    impl Service for Rig {
        fn start(&mut self) -> bool {
            true
        }
        fn local_id(&self) -> u64 {
            1
        }
        fn account(&self) -> u64 {
            2
        }
        fn local_name(&self) -> Vec<u8> {
            let mut name = Vec::new();
            unsafe { rig_local_name((&raw mut name).cast(), append) };
            name
        }
        fn send(&self, _: u64, _: u16, _: u16, _: &[u8]) -> bool {
            false
        }
        fn broadcast(&self, _: u16, _: u16, _: &[u8]) -> bool {
            false
        }
        fn receive(&self, _: u16, _: &mut Datagram) -> bool {
            false
        }
        fn invite_friends(&self) {
            unsafe { rig_invite_friends() }
        }
        fn friends(&self, list: &mut FriendList) -> bool {
            if !self.friends {
                return false;
            }
            let version = unsafe { rig_friends_version() };
            if list.version == version {
                return true;
            }
            let (mut playing, mut online, mut offline) = (0usize, 0i32, 0i32);
            unsafe { rig_friends_counts(&mut playing, &mut online, &mut offline) };
            *list = FriendList { version, playing: Vec::new(), online, offline };
            for at in 0..playing {
                let mut one = Friend::default();
                let mut elsewhere = 0;
                unsafe {
                    rig_friend(
                        at,
                        &mut one.id,
                        (&raw mut one.name).cast(),
                        append,
                        &mut one.with_you,
                        &mut elsewhere,
                        (&raw mut one.avatar).cast(),
                        &mut one.avatar_side,
                    )
                };
                one.elsewhere = ELSEWHERE[elsewhere as usize];
                list.playing.push(one);
            }
            true
        }
        fn invite_friend(&self, id: u64) -> bool {
            unsafe { rig_invite_friend(id) }
        }
        fn lobbies(&self) -> bool {
            true
        }
    }

    fn start(friends: bool, name: &'static std::ffi::CStr) -> bool {
        log_info!("online: the service is %s", name);
        let _ = online::SERVICE.set(Some(Started { service: Box::new(Rig { friends }), name }));
        online::get().is_some()
    }

    /// The services the C++ rig starts as steam and lan, said as the C++ says it.
    #[unsafe(no_mangle)]
    extern "C" fn rig_start_rig_steam() -> bool {
        start(true, c"steam")
    }

    #[unsafe(no_mangle)]
    extern "C" fn rig_start_rig_lan() -> bool {
        start(false, c"lan")
    }
}
