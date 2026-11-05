// Events, semaphores, mutants, timers and the waits on them: the kernel imports over
// objects.rs's dispatcher.

use super::objects::{
    self, CURRENT_THREAD_HANDLE, Event, Held, Kind, MAXIMUM_WAIT_OBJECTS, Mutant, Object, Timer, WaitList,
};
use super::{STATUS_INVALID_HANDLE, STATUS_INVALID_PARAMETER, STATUS_SUCCESS, STATUS_USER_APC, deliver_user_apcs};
use crate::guest::{Memory, PPCContext, Register, kernel_import};
use crate::log::log_warn;
#[cfg(feature = "diagnostics")]
use crate::runtime::{clock, pacing_trace, platform};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

#[cfg(feature = "diagnostics")]
mod blocked {
    // A backtrace of a stalled thread ends in a futex wait and stops being informative
    // exactly where the question starts -- which handle, and who else touches it. This
    // records that, so the watchdog can name it.
    use crate::log::format_text;
    use crate::runtime::{crash, lock_for_report, platform};
    use std::ffi::CStr;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    struct WaitRecord {
        handle: AtomicU32, // 0 when the thread is running
        caller: AtomicU32, // guest return address
        timed: AtomicBool,
        thread: Option<&'static CStr>,
    }

    // Each thread's record lives as long as the run, as the report may come after it.
    static RECORDS: Mutex<Vec<&'static WaitRecord>> = Mutex::new(Vec::new());

    thread_local! {
        static MINE: &'static WaitRecord = {
            let record: &'static WaitRecord = Box::leak(Box::new(WaitRecord {
                handle: AtomicU32::new(0),
                caller: AtomicU32::new(0),
                timed: AtomicBool::new(false),
                thread: crash::current_thread_name(),
            }));
            RECORDS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(record);
            record
        };
    }

    pub struct Blocked(&'static WaitRecord);

    impl Blocked {
        pub fn new(handle: u32, caller: u32, timed: bool) -> Self {
            let record = MINE.with(|record| *record);
            record.caller.store(caller, Ordering::Relaxed);
            record.timed.store(timed, Ordering::Relaxed);
            record.handle.store(handle, Ordering::Release);
            Self(record)
        }
    }

    impl Drop for Blocked {
        fn drop(&mut self) {
            self.0.handle.store(0, Ordering::Release);
        }
    }

    /// The threads blocked in a kernel wait, and on what: for the watchdog.
    pub fn report_waits() {
        let Some(records) = lock_for_report(&RECORDS) else { return };
        let mut any = false;
        for record in records.iter() {
            let handle = record.handle.load(Ordering::Acquire);
            if handle == 0 {
                continue;
            }
            if !any {
                platform::print_line(c"\n[E] ---- threads blocked in a wait ----");
            }
            any = true;
            platform::print_line(&format_text!(
                "[E] %-40s waiting on handle %08X%s, called from %08X",
                record.thread.unwrap_or(c"(unnamed)"),
                handle,
                if record.timed.load(Ordering::Relaxed) { c" (with a timeout)" } else { c" (forever)" },
                record.caller.load(Ordering::Relaxed)
            ));
        }
        if any {
            platform::print_line(c"[E] --------------------------------");
        }
    }
}

#[cfg(not(feature = "diagnostics"))]
mod blocked {
    pub struct Blocked;

    impl Blocked {
        pub fn new(_handle: u32, _caller: u32, _timed: bool) -> Self {
            Self
        }
    }
}

use blocked::Blocked;
#[cfg(feature = "diagnostics")]
pub use blocked::report_waits;

fn read_timeout(memory: Memory, address: u32) -> Option<i64> {
    (address != 0).then(|| memory.u64_at(address) as i64)
}

fn store_handle(memory: Memory, address: u32, handle: u32) {
    if address != 0 {
        memory.store_u32(address, handle);
    }
}

fn create(ctx: &mut PPCContext, memory: Memory, kind: Kind) {
    let handle = objects::lock().insert(Object::new(kind));
    store_handle(memory, ctx.r3.as_u32(), handle);
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
}

// What MW2_TRACE_PACING sees of a wait: when it began, from where, and when it ended.
pub(super) fn note(event: u8, value: u32) {
    #[cfg(feature = "diagnostics")]
    if pacing_trace::on() {
        pacing_trace::note(clock::nanoseconds(), event, value, platform::thread_id());
    }
    #[cfg(not(feature = "diagnostics"))]
    let _ = (event, value);
}

pub(super) fn note_callers(r1: u32) {
    #[cfg(feature = "diagnostics")]
    if pacing_trace::on() {
        pacing_trace::note_callers(|caller| note(pacing_trace::CALLER, caller), r1);
    }
    #[cfg(not(feature = "diagnostics"))]
    let _ = r1;
}

const KERNEL_WAIT: u8 = b'K';
pub(super) const KERNEL_WAIT_END: u8 = b'k';

// The wait itself, with what the watchdog and the pacing trace are told of it. That is
// done with the lock let go, as the C++ did: the list keeps the objects, and the trace's
// first note makes its table.
fn wait(ctx: &mut PPCContext, held: Held, list: &WaitList, all: bool, timeout: Option<i64>, first: u32) {
    drop(held);
    let deadline = objects::deadline(timeout);
    let caller = ctx.lr as u32;
    let _blocked = Blocked::new(first, caller, timeout.is_some());
    note(KERNEL_WAIT, caller);
    note_callers(ctx.r1.as_u32());
    ctx.r3.u64 = u64::from(objects::wait(objects::lock(), list, all, deadline));
    note(KERNEL_WAIT_END, 0);
}

kernel_import!(NtCreateEvent, |ctx, memory| {
    // (Handle*, ObjectAttributes, EventType, InitialState): type 0 is a notification
    // event (manual reset), 1 a synchronization event.
    let kind = Kind::Event(Event::new(ctx.r5.as_u32() == 0, ctx.r6.as_u32() != 0));
    create(ctx, memory, kind);
});

kernel_import!(NtSetEvent, |ctx, memory| {
    let held = objects::lock();
    let Some(event) = held.lookup(ctx.r3.as_u32()).filter(|object| object.as_event().is_some()) else {
        ctx.r3.u64 = u64::from(STATUS_INVALID_HANDLE);
        return;
    };
    let was = objects::set_event_held(&held, event);
    drop(held);
    store_handle(memory, ctx.r4.as_u32(), was.into());
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

kernel_import!(NtClearEvent, |ctx, _memory| {
    let held = objects::lock();
    let Some(event) = held.lookup(ctx.r3.as_u32()).filter(|object| object.as_event().is_some()) else {
        ctx.r3.u64 = u64::from(STATUS_INVALID_HANDLE);
        return;
    };
    objects::reset_event_held(&held, event);
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

kernel_import!(NtCreateMutant, |ctx, memory| {
    // (Handle*, ObjectAttributes, InitialOwner)
    let kind = Kind::Mutant(Mutant::new(ctx.r5.as_u32() != 0));
    create(ctx, memory, kind);
});

kernel_import!(NtReleaseMutant, |ctx, memory| {
    // (Handle, PreviousCount*): the count is NT's signal state, one when free and one
    // less for every acquisition outstanding.
    let held = objects::lock();
    let Some(mutant) = held.lookup(ctx.r3.as_u32()).filter(|object| matches!(object.kind, Kind::Mutant(_))) else {
        ctx.r3.u64 = u64::from(STATUS_INVALID_HANDLE);
        return;
    };
    let depth = objects::release_mutant(&held, mutant);
    drop(held);
    store_handle(memory, ctx.r4.as_u32(), 1i32.wrapping_sub(depth) as u32);
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

kernel_import!(NtCreateTimer, |ctx, memory| {
    // (Handle*, ObjectAttributes, TimerType): 0 notification, 1 synchronization.
    let kind = Kind::Timer(Timer::new(ctx.r5.as_u32() == 0, false));
    create(ctx, memory, kind);
});

static TIMER_ROUTINES_DROPPED: AtomicU32 = AtomicU32::new(0);

kernel_import!(NtSetTimerEx, |ctx, memory| {
    // (Handle, DueTime*, ApcRoutine, ApcMode, ApcContext, Resume, PeriodMs, 0)
    let Some(timer) = lookup(ctx.r3.as_u32(), |object| matches!(object.kind, Kind::Timer(_))) else {
        ctx.r3.u64 = u64::from(STATUS_INVALID_HANDLE);
        return;
    };
    if ctx.r4.as_u32() == 0 {
        ctx.r3.u64 = u64::from(STATUS_INVALID_PARAMETER);
        return;
    }
    // A completion routine would be queued to the thread that set the timer. Nothing in
    // either title passes one; say so if that changes.
    if ctx.r5.as_u32() != 0 && TIMER_ROUTINES_DROPPED.fetch_add(1, Ordering::Relaxed) == 0 {
        log_warn!(
            "NtSetTimerEx: timer %08X has a completion routine %08X, which is not called",
            ctx.r3.as_u32(),
            ctx.r5.as_u32()
        );
    }
    objects::set_timer(&timer, memory.u64_at(ctx.r4.as_u32()) as i64, ctx.r9.as_u32());
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

kernel_import!(NtCancelTimer, |ctx, memory| {
    // (Handle, CurrentState*)
    let held = objects::lock();
    let Some(timer) = held.lookup(ctx.r3.as_u32()).filter(|object| matches!(object.kind, Kind::Timer(_))) else {
        ctx.r3.u64 = u64::from(STATUS_INVALID_HANDLE);
        return;
    };
    let was = objects::cancel_timer(&held, timer);
    drop(held);
    store_handle(memory, ctx.r4.as_u32(), was.into());
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

// The object a handle names, if it is of the kind asked for.
fn lookup(handle: u32, wanted: impl Fn(&Object) -> bool) -> Option<Arc<Object>> {
    objects::lock().lookup(handle).filter(|object| wanted(object)).cloned()
}

kernel_import!(NtWaitForSingleObjectEx, |ctx, memory| {
    // (Handle, WaitMode, Alertable, Timeout*). An alertable wait is a delivery point
    // for user APCs, and asynchronous reads only transfer when their APC runs -- so
    // skipping delivery here can leave the title reading a buffer nothing ever filled.
    // Only the thread itself queues its APCs, so none can arrive once it is asleep.
    if ctx.r5.as_u32() != 0 && deliver_user_apcs(ctx, memory) {
        ctx.r3.u64 = u64::from(STATUS_USER_APC);
        return;
    }
    let handle = ctx.r3.as_u32();
    let held = objects::lock();
    let mut list = WaitList::new();
    let Some(object) = held.lookup(handle) else {
        ctx.r3.u64 = u64::from(STATUS_INVALID_HANDLE);
        return;
    };
    list.add(object);
    let timeout = read_timeout(memory, ctx.r6.as_u32());
    wait(ctx, held, &list, false, timeout, handle);
});

kernel_import!(NtWaitForMultipleObjectsEx, |ctx, memory| {
    // (Count, Handles*, WaitType, WaitMode, Alertable, Timeout*). WaitType 0 waits for
    // all of them, 1 for any. A loader thread waits here for "work or shutdown", so this
    // is an APC delivery point as the single wait is.
    let count = ctx.r3.as_u32();
    let handles = ctx.r4.as_u32();
    if ctx.r7.as_u32() != 0 && deliver_user_apcs(ctx, memory) {
        ctx.r3.u64 = u64::from(STATUS_USER_APC);
        return;
    }
    if handles == 0 || count == 0 || count as usize > MAXIMUM_WAIT_OBJECTS {
        ctx.r3.u64 = u64::from(STATUS_INVALID_PARAMETER);
        return;
    }
    let held = objects::lock();
    let mut list = WaitList::new();
    for i in 0..count {
        let Some(object) = held.lookup(memory.u32_at(handles.wrapping_add(i * 4))) else {
            ctx.r3.u64 = u64::from(STATUS_INVALID_HANDLE);
            return;
        };
        list.add(object);
    }
    let timeout = read_timeout(memory, ctx.r8.as_u32());
    let first = memory.u32_at(handles);
    wait(ctx, held, &list, ctx.r5.as_u32() == 0, timeout, first);
});

kernel_import!(KeSetEvent, |ctx, memory| {
    // (Event, Increment, Wait): returns the previous state.
    let mut held = objects::lock();
    let was = match held.object_at(memory, ctx.r3.as_u32()) {
        Some(event) if event.as_event().is_some() => objects::set_event_held(&held, &event),
        _ => false,
    };
    ctx.r3.u64 = was.into();
});

kernel_import!(KeResetEvent, |ctx, memory| {
    let mut held = objects::lock();
    let was = match held.object_at(memory, ctx.r3.as_u32()) {
        Some(event) => objects::reset_event_held(&held, &event),
        None => false,
    };
    ctx.r3.u64 = was.into();
});

kernel_import!(KeWaitForSingleObject, |ctx, memory| {
    // (Object, WaitReason, WaitMode, Alertable, Timeout*)
    if ctx.r6.as_u32() != 0 && deliver_user_apcs(ctx, memory) {
        ctx.r3.u64 = u64::from(STATUS_USER_APC);
        return;
    }
    let address = ctx.r3.as_u32();
    let mut held = objects::lock();
    let mut list = WaitList::new();
    let Some(object) = held.object_at(memory, address) else {
        ctx.r3.u64 = u64::from(STATUS_INVALID_PARAMETER);
        return;
    };
    list.add(&object);
    let timeout = read_timeout(memory, ctx.r7.as_u32());
    wait(ctx, held, &list, false, timeout, address);
});

// KeWaitForMultipleObjects(Count, Object[], WaitType, WaitReason, WaitMode, Alertable,
//                          Timeout*, WaitBlockArray)
// XAudio2's mixer hands its work to a thread of its own and waits here for either
// "done" or "stop", so the wait type and the index returned both matter.
kernel_import!(KeWaitForMultipleObjects, |ctx, memory| {
    let count = ctx.r3.as_u32();
    let addresses = ctx.r4.as_u32();
    if ctx.r8.as_u32() != 0 && deliver_user_apcs(ctx, memory) {
        ctx.r3.u64 = u64::from(STATUS_USER_APC);
        return;
    }
    if addresses == 0 || count == 0 || count as usize > MAXIMUM_WAIT_OBJECTS {
        ctx.r3.u64 = u64::from(STATUS_INVALID_PARAMETER);
        return;
    }
    let mut held = objects::lock();
    let mut list = WaitList::new();
    for i in 0..count {
        let Some(object) = held.object_at(memory, memory.u32_at(addresses.wrapping_add(i * 4))) else {
            ctx.r3.u64 = u64::from(STATUS_INVALID_PARAMETER);
            return;
        };
        list.add(&object);
    }
    let timeout = read_timeout(memory, ctx.r9.as_u32());
    let first = memory.u32_at(addresses);
    wait(ctx, held, &list, ctx.r5.as_u32() == 0, timeout, first);
});

kernel_import!(KeInitializeSemaphore, |ctx, memory| {
    // (Semaphore, Count, Limit), written into the header as the kernel does; the wait
    // list pointing at itself makes it a new object.
    let address = ctx.r3.as_u32();
    memory.store_u8(address, objects::SEMAPHORE);
    memory.store_u32(address.wrapping_add(4), ctx.r4.as_u32());
    memory.store_u32(address.wrapping_add(8), address.wrapping_add(8));
    memory.store_u32(address.wrapping_add(12), address.wrapping_add(8));
    memory.store_u32(address.wrapping_add(16), ctx.r5.as_u32());
    objects::object_at(memory, address);
});

kernel_import!(KeReleaseSemaphore, |ctx, memory| {
    // (Semaphore, Increment, Adjustment, Wait): returns the previous count.
    let mut held = objects::lock();
    ctx.r3.u64 = match held.object_at(memory, ctx.r3.as_u32()) {
        Some(semaphore) if matches!(semaphore.kind, Kind::Semaphore(_)) => {
            u64::from(objects::release_semaphore(&held, &semaphore, ctx.r5.as_i32()) as u32)
        }
        _ => 0,
    };
});

kernel_import!(NtClose, |ctx, _memory| {
    let closed = objects::lock().close(ctx.r3.as_u32());
    ctx.r3.u64 = u64::from(if closed.is_some() { STATUS_SUCCESS } else { STATUS_INVALID_HANDLE });
    // An object of the C++ goes here, with no lock held.
    drop(closed);
});

kernel_import!(NtDuplicateObject, |ctx, memory| {
    let mut held = objects::lock();
    let Some(object) = held.lookup(ctx.r3.as_u32()).cloned() else {
        ctx.r3.u64 = u64::from(STATUS_INVALID_HANDLE);
        return;
    };
    if ctx.r4.as_u32() != 0 {
        let handle = held.insert(object);
        drop(held);
        memory.store_u32(ctx.r4.as_u32(), handle);
    }
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

kernel_import!(ObReferenceObjectByHandle, |ctx, memory| {
    // (Handle, ObjectType, Object*): the object's dispatcher header, which the Ke* calls
    // take. The title references each thread it creates this way to set its priority
    // and start it.
    const CURRENT_THREAD_OFFSET: u32 = 256; // KPCR.CurrentThread
    let header = if ctx.r3.as_u32() == CURRENT_THREAD_HANDLE {
        memory.u32_at(ctx.r13.as_u32().wrapping_add(CURRENT_THREAD_OFFSET))
    } else {
        let object = objects::lock().lookup(ctx.r3.as_u32()).cloned();
        object.map_or(0, |object| objects::header_of(memory, &object))
    };
    store_handle(memory, ctx.r5.as_u32(), header);
    ctx.r3.u64 = u64::from(if header != 0 { STATUS_SUCCESS } else { STATUS_INVALID_HANDLE });
});

kernel_import!(ObDereferenceObject, |ctx, _memory| {
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

kernel_import!(ObCreateSymbolicLink, |ctx, _memory| {
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

kernel_import!(ObDeleteSymbolicLink, |ctx, _memory| {
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});
