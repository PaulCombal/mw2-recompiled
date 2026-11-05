// The engine's generic "spin until this predicate says yes", where every stall in the job
// system and the dynamic buffers ends up. Diagnostics only.
//
// A backtrace through it says a thread is blocked; only the predicate says on what, and
// the time each thread spent in it by predicate and caller says where its time went.

use crate::guest::{Register, guest_hook};
use crate::log::log_info;
use crate::runtime::{clock, crash, pacing_trace, platform};
use std::ffi::CStr;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicU32, AtomicU64};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

// How long a thread has spent waiting, by predicate and caller: a thread stuck at the end
// of a run says where it was, not where its time went.
#[derive(Default)]
struct Spent {
    predicate: AtomicU32,
    caller: AtomicU32,
    nanoseconds: AtomicU64,
    calls: AtomicU64,
}

const SPENT: usize = 16;

struct PredicateWait {
    predicate: AtomicU32, // 0 when not waiting
    caller: AtomicU32,
    laps: AtomicU64,
    thread: Option<&'static CStr>,
    spent: [Spent; SPENT], // written by its own thread only
}

// Each thread's lives as long as the run, as the report comes after some have ended.
static WAITS: Mutex<Vec<&'static PredicateWait>> = Mutex::new(Vec::new());

thread_local! {
    static MINE: &'static PredicateWait = {
        let mine: &'static PredicateWait = Box::leak(Box::new(PredicateWait {
            predicate: AtomicU32::new(0),
            caller: AtomicU32::new(0),
            laps: AtomicU64::new(0),
            thread: crash::current_thread_name(),
            spent: Default::default(),
        }));
        WAITS.lock().unwrap_or_else(PoisonError::into_inner).push(mine);
        mine
    };
}

const WAIT_BEGIN: u8 = b'W';
const WAIT_END: u8 = b'R';
const THREAD_TIME: u8 = b'C';

fn note(event: u8, value: u32) {
    pacing_trace::note(clock::nanoseconds(), event, value, platform::thread_id());
}

fn thread_microseconds() -> u32 {
    (platform::thread_cpu_time() / 1000) as u32
}

guest_hook!(T_Job_WaitPredicate, |ctx, memory| {
    let wait = MINE.with(|mine| *mine);
    let predicate = ctx.r3.as_u32();
    let caller = ctx.lr as u32;
    wait.caller.store(caller, Relaxed);
    wait.laps.fetch_add(1, Relaxed);
    wait.predicate.store(predicate, Release);
    let began = Instant::now();
    let traced = pacing_trace::on();
    if traced {
        note(WAIT_BEGIN, predicate);
        note(THREAD_TIME, thread_microseconds());
    }
    crate::title::T_Job_WaitPredicate.original(ctx, memory);
    if traced {
        note(WAIT_END, predicate);
        note(THREAD_TIME, thread_microseconds());
    }
    let took = began.elapsed().as_nanos() as u64;
    wait.predicate.store(0, Release);
    for one in &wait.spent {
        let had = one.predicate.load(Relaxed);
        if had != 0 && (had != predicate || one.caller.load(Relaxed) != caller) {
            continue;
        }
        if had == 0 {
            one.caller.store(caller, Relaxed);
            one.predicate.store(predicate, Release);
        }
        one.nanoseconds.fetch_add(took, Relaxed);
        one.calls.fetch_add(1, Relaxed);
        break;
    }
});

/// At the end of a run: where the waiting went, and who is waiting still (runtime/engine.h).
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub(super) extern "C" fn mw2_engine_report_predicate_waits() {
    let waits = WAITS.lock().unwrap_or_else(PoisonError::into_inner);
    let unnamed = c"(unnamed)";
    let mut lines: Vec<(&CStr, u32, u32, u64, u64)> = Vec::new();
    for wait in waits.iter() {
        for one in &wait.spent {
            if one.predicate.load(Acquire) != 0 && one.nanoseconds.load(Relaxed) >= 100_000_000 {
                lines.push((
                    wait.thread.unwrap_or(unnamed),
                    one.predicate.load(Relaxed),
                    one.caller.load(Relaxed),
                    one.nanoseconds.load(Relaxed),
                    one.calls.load(Relaxed),
                ));
            }
        }
    }
    lines.sort_by_key(|line| std::cmp::Reverse(line.3));
    if !lines.is_empty() {
        log_info!("time spent waiting on engine predicates, 100 ms or more:");
    }
    for (thread, predicate, caller, nanoseconds, calls) in lines {
        log_info!(
            "  %-40s %7llu ms in %llu waits on sub_%08X from %08X",
            thread,
            nanoseconds / 1_000_000,
            calls,
            predicate,
            caller
        );
    }
    let mut any = false;
    for wait in waits.iter() {
        let predicate = wait.predicate.load(Acquire);
        if predicate == 0 {
            continue;
        }
        if !any {
            log_info!("threads waiting on an engine predicate:");
        }
        any = true;
        log_info!(
            "  %-40s predicate sub_%08X, called from %08X, %llu waits so far",
            wait.thread.unwrap_or(unnamed),
            predicate,
            wait.caller.load(Relaxed),
            wait.laps.load(Relaxed)
        );
    }
}
