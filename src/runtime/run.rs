// A run of the game, from the executable's start to its end: what runtime/main.cpp
// does in the C++ build, in the same order. src/main.rs hands it the command line.
//
// The guest never returns, so a run ends in request_exit (the window closed, the
// guide's quit, a deadline) or on a fault, and both write the same end-of-run reports.

use crate::env;
use crate::guest::{Register, blank_context, load_float_mode};
use crate::log::{log_error, log_info, log_warn};
use crate::runtime::bridge::run as renderer;
use crate::runtime::install::{self, spawn::c_path};
use crate::runtime::{crash, guest_memory, kernel, online, platform, report, settings};
use crate::title::{T_ENTRY_POINT, TITLE_NAME};
use ppc_rt::{PPC_CODE_BASE, PPC_CODE_SIZE, PPC_IMAGE_BASE, PPC_IMAGE_SIZE};
use std::ffi::CStr;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

/// The reports a run ends with. A run that ends on a deadline and a run that ends on a
/// fault are asking the same question, so they print the same ones.
pub fn report_all() {
    report::write(&report::System);
    #[cfg(feature = "diagnostics")]
    {
        use crate::runtime::{pacing_trace, player};
        kernel::report_unimplemented();
        kernel::files::report_missing_files();
        kernel::report_memory();
        renderer::gpu_report();
        player::report();
        crate::hooks::report_predicate_waits();
        if let Some(path) = pacing_trace::path() {
            pacing_trace::write(path);
        }
        kernel::xma::report();
    }
    // A player's report too: underruns are what a crackle in it would be.
    kernel::report_audio();
}

/// What a fault prints before the process ends (crash.rs).
pub fn on_fault() {
    report_all();
}

/// One shutdown however it is asked for, and on a thread of its own: the caller may be
/// the window thread, which stopping the presenter has to join. The first ask is the
/// one that counts.
pub fn request_exit(why: &CStr) {
    static ASKED: AtomicBool = AtomicBool::new(false);
    if ASKED.swap(true, Ordering::SeqCst) {
        return;
    }
    let why = why.to_owned();
    spawn(move || {
        log_info!("--------------------------------------------------");
        log_info!("exiting: %s", why.as_c_str());
        #[cfg(feature = "diagnostics")]
        crate::runtime::stutters::ending();
        renderer::gpu_shutdown();
        report_all();
        install::start_next_title();
        platform::exit_now(0);
    });
}

// A thread of the runtime's that ends the run or waits for that: the stack a C++
// std::thread got, as what it runs (the renderer's shutdown, the reports) was written
// for that one. Never joined. One that cannot start ends the process, as a
// std::thread's did: a run that could no longer end tidily would be one that a
// closed window leaves running.
fn spawn(body: impl FnOnce() + Send + 'static) {
    let started = thread::Builder::new().stack_size(platform::default_thread_stack()).spawn(body);
    if started.is_err() {
        log_error!("could not start a thread of the runtime");
        std::process::abort();
    }
}

// The guest never returns, so the reports at the end of run() are unreachable and
// killing the process loses everything measured. MW2_RUN_SECONDS=<n> reports and exits
// after n seconds.
#[cfg(feature = "diagnostics")]
fn start_deadline() {
    let seconds = env::number(c"MW2_RUN_SECONDS", 0) as u32;
    if seconds == 0 {
        return;
    }
    log_info!("run deadline: reporting and exiting after %u seconds", seconds);
    spawn(move || {
        thread::sleep(std::time::Duration::from_secs(u64::from(seconds)));
        // The guest threads are still running over state the reports walk; unwinding
        // them cleanly is not worth it for a measurement.
        request_exit(c"the run deadline was reached");
    });
}

// MW2_QUIT_AFTER_ARRIVAL=<n> ends the run n seconds after MW2_WALK_TO gets where it was
// sent. A walk that takes four minutes and a measurement that takes ten seconds should
// not be followed by two minutes of standing there.
#[cfg(feature = "diagnostics")]
fn start_arrival_deadline() {
    use crate::runtime::{clock, player};
    use std::time::Duration;
    if env::text(c"MW2_QUIT_AFTER_ARRIVAL").is_none() {
        return;
    }
    let seconds = env::number(c"MW2_QUIT_AFTER_ARRIVAL", 0) as u32;
    log_info!("run deadline: reporting and exiting %u seconds after arriving", seconds);
    // A walk that has stalled against a wall is as finished as one that arrived -- it
    // will take no more measurements. Ending there rather than at the watchdog saves
    // the rest of the run, which is most of it when the route snags early.
    let given = env::real(c"MW2_QUIT_IF_STUCK", 0.0);
    let stall = if given > 0.0 { given } else { 45.0 };
    log_info!("run deadline: or %0.f seconds after the walk stops getting nearer", stall);
    spawn(move || {
        let mut stuck = false;
        while !player::arrived() {
            if player::seconds_since_progress(clock::nanoseconds()) > stall {
                stuck = true;
                break;
            }
            thread::sleep(Duration::from_millis(250));
        }
        thread::sleep(Duration::from_secs(u64::from(seconds)));
        request_exit(if stuck {
            c"the walk stopped getting anywhere"
        } else {
            c"the walk arrived and its measurement is done"
        });
    });
}

fn load_image(image: &[u8]) {
    if image.len() as u64 > PPC_IMAGE_SIZE {
        log_warn!(
            "image is %llu bytes, larger than PPC_IMAGE_SIZE %llu; truncating",
            image.len() as u64,
            PPC_IMAGE_SIZE
        );
    }
    let size = image.len().min(PPC_IMAGE_SIZE as usize);
    guest_memory::memory().store_bytes(PPC_IMAGE_BASE as u32, &image[..size]);
    log_info!("loaded %llu bytes at guest %08X", size as u64, PPC_IMAGE_BASE as u32);
}

// The recompiled functions into the table PPC_LOOKUP_FUNC reads.
fn build_function_table() -> u64 {
    let mut entries = 0;
    for mapping in &ppc_recomp::PPCFuncMappings {
        if mapping.guest == 0 && mapping.host.is_none() {
            break;
        }
        let Some(host) = mapping.host else { continue };
        let guest = mapping.guest as u64;
        if !(PPC_CODE_BASE..PPC_CODE_BASE + PPC_CODE_SIZE).contains(&guest)
            || !guest_memory::set_function(guest as u32, host)
        {
            log_warn!("function %08llX outside code range, skipped", guest);
            continue;
        }
        entries += 1;
    }
    entries
}

// MW2_LOG_FILE=<path> sends the log there instead of the terminal. Every line this
// runtime writes goes to stderr, so a plain `> file` redirection catches nothing. Its
// folder is made first: a run started with a new folder named lost its whole log to
// the terminal. A title the other one started (from its menus) carries on in the log
// of the run it belongs to (MW2_LOG_APPEND).
fn send_log_to_file() {
    let Some(path) = env::text(c"MW2_LOG_FILE") else { return };
    if let Some(folder) = platform::system_path(path).parent().filter(|folder| !folder.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(folder);
    }
    if !platform::send_log_to(path, env::flag(c"MW2_LOG_APPEND")) {
        let _ = writeln!(std::io::stdout(), "could not open {} for the log", path.to_string_lossy());
    }
}

/// The run, from the command line (src/main.rs): the exit code, when the run ends by
/// returning rather than by request_exit.
pub fn run(arguments: &[&CStr]) -> i32 {
    let argc = arguments.len() as i32;
    platform::initialise();

    // Before any switch is read, the log's among them.
    let kept = settings::load(&install::folder(argc).join(".env"));
    send_log_to_file();

    log_info!("--- MW2 recompilation runtime (%s) ---", TITLE_NAME);
    if !kept.is_empty() {
        log_info!("settings: %s", crate::log::c_string(kept).as_c_str());
    }

    // Before any thread exists: a Flatpak's Steam is reached from its own process
    // namespace, which a process of more than one thread cannot join. The parent that
    // waited for the game ends as it did, and runs nothing of the game's on the way out.
    if let Some(code) = online::enter_service_namespace() {
        platform::exit_now(code);
    }

    // A player starts the executable with no arguments (install/); a development run
    // names the image, a flat PE or the XEX, and the game folder.
    let player_start = argc == 1;
    let (image, game_root): (Vec<u8>, PathBuf) = if player_start {
        let mut code = 0;
        let Some(image) = install::prepare(&mut code) else { return code };
        // The launcher compiles every shader of the game into this cache before the
        // game first runs (launcher/precompile.cpp), and the game draws only with that
        // done for the device it draws on.
        let game_root = PathBuf::from("game");
        renderer::use_compiled_shaders(&c_path(&game_root));
        (image, game_root)
    } else {
        let Some(path) = arguments.get(1) else { return 1 };
        let Some(image) = install::load_image_file(&platform::system_path(path)) else { return 1 };
        let game_root = arguments.get(2).map_or_else(|| PathBuf::from("mw2/game"), |root| platform::system_path(root));
        (image, game_root)
    };

    crash::install();
    // Logged in before any other thread runs: Steam reads the app id from the
    // environment, which the service sets.
    let _ = online::get();

    if guest_memory::initialise().is_null() {
        return 1;
    }
    load_image(&image);
    drop(image);
    #[cfg(all(feature = "diagnostics", not(windows)))]
    crate::runtime::watchpoint::install();

    let mapped = build_function_table();
    log_info!("function table: %llu entries", mapped);

    crash::register_thread(c"main thread");
    #[cfg(feature = "diagnostics")]
    crate::runtime::player::start_recording();
    kernel::initialise(&c_path(&game_root));
    renderer::gpu_initialise();
    // The device is known now, and nothing of the title has run: the launcher compiles
    // the shaders for it and starts the game again. Ended at once, without writing this
    // run's pipeline cache over the one the launcher is about to make.
    if player_start {
        let (compiled, why, device) = renderer::compiled_shaders();
        if !compiled && install::compile_shaders(&crate::log::c_string(device), &crate::log::c_string(why)) {
            platform::exit_now(0);
        }
    }
    renderer::show_window();
    kernel::xma::initialise();

    // PPC grows down; leave a zeroed backchain at the top.
    const STACK_SIZE: u32 = 1 << 20;
    let stack_base = kernel::allocate_guest(STACK_SIZE, 0x1000);
    if stack_base == 0 {
        log_error!("could not allocate guest stack");
        return 1;
    }
    let stack_top = (stack_base + STACK_SIZE - 0x100) & !0xF;

    // r13 is the Xbox 360 thread pointer (KPCR). The recompiled code makes ~1100
    // r13-relative accesses and reads its own thread id out of KPCR.CurrentThread ->
    // KTHREAD.ThreadId, so the block must be populated.
    let pcr = kernel::create_thread_block(1);
    if pcr == 0 {
        log_error!("could not allocate the thread block");
        return 1;
    }

    let mut ctx = blank_context();
    ctx.r1.u64 = u64::from(stack_top);
    ctx.r13.u64 = u64::from(pcr);
    load_float_mode(&mut ctx);

    let memory = guest_memory::memory();
    let entry = T_ENTRY_POINT; // the XEX header's ENTRY_POINT, from title.h
    let Some(function) = memory.function_at(entry) else {
        log_error!("no recompiled function at entry point %08X", entry);
        return 1;
    };

    log_info!("thread block (r13) at %08X", pcr);
    #[cfg(feature = "diagnostics")]
    {
        start_deadline();
        start_arrival_deadline();
    }
    log_info!("entering guest at %08X (stack %08X..%08X)", entry, stack_base, stack_top);
    log_info!("--------------------------------------------------");
    function.call(&mut ctx, memory);
    log_info!("--------------------------------------------------");
    log_info!("guest returned, r3 = %08X", ctx.r3.as_u32());

    renderer::gpu_shutdown();
    report_all();
    0
}
