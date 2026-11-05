// Kernel objects and the waits on them.
//
// Nt* calls name an object by handle and Ke* calls by the address of its dispatcher
// header in guest memory. On the console the two are one object --
// ObReferenceObjectByHandle turns a handle into that address -- and here too: every
// object lives in the handle table, and a header in guest memory names its object by
// writing the handle and a signature into its wait list, as Xenia does. A header the
// title initialises (or initialises again) inline has its own wait list there instead,
// and is taken up as a new object with the type and signal state it holds.
//
// Waits follow NT's dispatcher: one lock guards every object's state, a wait on several
// objects is satisfied atomically, and a waiting thread sleeps until something it waits
// on is signalled rather than polling. The same lock holds the handle table and the
// headers, so that a call takes one lock, not three as the C++ did.
//
// An object's state is atomics only so that it can be shared: it is read and changed
// with the lock held, and so Relaxed.

use super::{STATUS_SUCCESS, STATUS_TIMEOUT, allocate_guest, system_time};
use crate::guest::Memory;
use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

/// The pseudo-handle the kernel defines for "the calling thread".
pub const CURRENT_THREAD_HANDLE: u32 = 0xFFFF_FFFF;

pub struct Object {
    pub kind: Kind,
    header: AtomicU32, // the dispatcher header, once it has one
    waiting: AtomicU32,
    waiters: Mutex<Vec<Arc<Waiter>>>, // taken only with the dispatcher lock held
}

pub enum Kind {
    Event(Event),
    Semaphore(Semaphore),
    Mutant(Mutant),
    Timer(Timer),
    Thread(super::threads::GuestThread),
    /// What the C++ keeps in the table (files, notification listeners, sessions,
    /// enumerators): always signalled to a wait, and nothing to a Ke call.
    Other(Box<dyn Any + Send + Sync>),
}

pub struct Event {
    signalled: AtomicBool,
    manual_reset: bool, // a notification event; false is synchronization
}

pub struct Semaphore {
    count: AtomicI32,
    limit: i32,
}

/// Owned by one thread at a time, which may take it again as often as it likes and has
/// to release it as often.
pub struct Mutant {
    owner: AtomicU64,
    depth: AtomicU32,
}

/// Signalled when it falls due, and again every period after if it has one.
pub struct Timer {
    event: Event,
    generation: AtomicU64, // a later set or cancel overrides an earlier one
    running: AtomicBool,
}

impl Event {
    pub fn new(manual_reset: bool, signalled: bool) -> Self {
        Self { signalled: AtomicBool::new(signalled), manual_reset }
    }
}

impl Semaphore {
    pub fn new(count: i32, limit: i32) -> Self {
        Self { count: AtomicI32::new(count), limit }
    }
}

impl Mutant {
    pub fn new(owned: bool) -> Self {
        Self { owner: AtomicU64::new(if owned { current_thread() } else { 0 }), depth: AtomicU32::new(owned.into()) }
    }
}

impl Timer {
    pub fn new(manual_reset: bool, signalled: bool) -> Self {
        Self {
            event: Event::new(manual_reset, signalled),
            generation: AtomicU64::new(0),
            running: AtomicBool::new(false),
        }
    }
}

impl Object {
    pub fn new(kind: Kind) -> Arc<Self> {
        Arc::new(Self { kind, header: AtomicU32::new(0), waiting: AtomicU32::new(0), waiters: Mutex::new(Vec::new()) })
    }

    /// An event, or a timer, which is one.
    pub fn as_event(&self) -> Option<&Event> {
        match &self.kind {
            Kind::Event(event) => Some(event),
            Kind::Timer(timer) => Some(&timer.event),
            _ => None,
        }
    }

    pub fn as_thread(&self) -> Option<&super::threads::GuestThread> {
        match &self.kind {
            Kind::Thread(thread) => Some(thread),
            _ => None,
        }
    }

    fn is_dispatcher(&self) -> bool {
        !matches!(self.kind, Kind::Other(_))
    }

    fn signalled(&self, thread: u64) -> bool {
        match &self.kind {
            Kind::Event(event) => event.signalled.load(Relaxed),
            Kind::Timer(timer) => timer.event.signalled.load(Relaxed),
            Kind::Semaphore(semaphore) => semaphore.count.load(Relaxed) > 0,
            Kind::Mutant(mutant) => {
                let owner = mutant.owner.load(Relaxed);
                owner == 0 || owner == thread
            }
            Kind::Thread(guest) => guest.exited.load(Relaxed),
            Kind::Other(_) => true,
        }
    }

    // What satisfying a wait does: a synchronization event resets, a semaphore counts
    // down, a mutant is taken.
    fn satisfy(&self, thread: u64) {
        match &self.kind {
            Kind::Event(event) | Kind::Timer(Timer { event, .. }) => {
                if !event.manual_reset {
                    event.signalled.store(false, Relaxed);
                }
            }
            Kind::Semaphore(semaphore) => {
                semaphore.count.store(semaphore.count.load(Relaxed).wrapping_sub(1), Relaxed);
            }
            Kind::Mutant(mutant) => {
                mutant.owner.store(thread, Relaxed);
                mutant.depth.store(mutant.depth.load(Relaxed).wrapping_add(1), Relaxed);
            }
            Kind::Thread(_) | Kind::Other(_) => {}
        }
    }

    // Wakes every thread waiting on this object to look again.
    fn wake(&self) {
        if self.waiting.load(Relaxed) != 0 {
            for waiter in self.waiters.lock().unwrap_or_else(PoisonError::into_inner).iter() {
                waiter.wake();
            }
        }
    }

    fn add_waiter(&self, waiter: &Arc<Waiter>) {
        self.waiters.lock().unwrap_or_else(PoisonError::into_inner).push(Arc::clone(waiter));
        self.waiting.store(self.waiting.load(Relaxed) + 1, Relaxed);
    }

    fn remove_waiter(&self, waiter: &Arc<Waiter>) {
        let mut waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(at) = waiters.iter().position(|one| Arc::ptr_eq(one, waiter)) {
            waiters.swap_remove(at);
            self.waiting.store(self.waiting.load(Relaxed) - 1, Relaxed);
        }
    }

    // The header type an object is given when ObReferenceObjectByHandle asks for one.
    fn header_type(&self) -> u8 {
        match &self.kind {
            Kind::Timer(timer) if timer.event.manual_reset => NOTIFICATION_TIMER,
            Kind::Timer(_) => SYNCHRONIZATION_TIMER,
            Kind::Event(event) if event.manual_reset => NOTIFICATION_EVENT,
            Kind::Event(_) => SYNCHRONIZATION_EVENT,
            Kind::Semaphore(_) => SEMAPHORE,
            Kind::Mutant(_) => MUTANT,
            Kind::Thread(_) | Kind::Other(_) => THREAD, // threads, and anything else a handle names
        }
    }
}

// The caller's identity for mutant ownership: stable for the thread's life, and never
// given to another. (The C++ build's was the address of a thread-local, which a later
// thread can be given, and with it a mutant a thread that ended still held.)
pub fn current_thread() -> u64 {
    use std::cell::Cell;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    thread_local! { static ME: Cell<u64> = const { Cell::new(0) }; }
    ME.with(|me| {
        if me.get() == 0 {
            me.set(NEXT.fetch_add(1, Relaxed));
        }
        me.get()
    })
}

// X_DISPATCHER_HEADER: the type byte at +0, the signal state at +4 and the wait list at
// +8 (flink) and +12 (blink); a semaphore's limit follows at +16. A header naming one of
// the objects here holds its id in the flink and this in the blink -- which the title's
// own initialisation, pointing the list at itself, overwrites.
const SIGNATURE: u32 = 0x4D573200; // 'MW2\0'
const NOTIFICATION_EVENT: u8 = 0;
const SYNCHRONIZATION_EVENT: u8 = 1;
const MUTANT: u8 = 2;
pub const SEMAPHORE: u8 = 5;
pub const THREAD: u8 = 6;
const NOTIFICATION_TIMER: u8 = 8;
const SYNCHRONIZATION_TIMER: u8 = 9;

// Handles and header ids are small numbers handed out in order.
#[derive(Default)]
struct Ids(u64);

impl Hasher for Ids {
    fn finish(&self) -> u64 {
        self.0.wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(26)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 << 8) | u64::from(byte);
        }
    }

    fn write_u32(&mut self, value: u32) {
        self.0 = u64::from(value);
    }
}

type ById = HashMap<u32, Arc<Object>, BuildHasherDefault<Ids>>;

pub struct Table {
    handles: ById,
    next_handle: u32,
    headers: ById, // by id
    next_id: u32,
}

static TABLE: Mutex<Table> = Mutex::new(Table {
    handles: HashMap::with_hasher(BuildHasherDefault::new()),
    next_handle: 0x0001_0004,
    headers: HashMap::with_hasher(BuildHasherDefault::new()),
    next_id: 1,
});

pub type Held = MutexGuard<'static, Table>;

/// The dispatcher lock. Held while reading or changing an object's state, the handle
/// table or the headers; never across a call into the guest.
pub fn lock() -> Held {
    // Nothing panics with it held but by aborting.
    TABLE.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Table {
    pub fn insert(&mut self, object: Arc<Object>) -> u32 {
        // A handle still open after the numbers come round is not given again.
        let mut handle = self.next_handle;
        while self.handles.contains_key(&handle) {
            handle = handle.wrapping_add(4);
        }
        self.next_handle = handle.wrapping_add(4);
        self.handles.insert(handle, object);
        handle
    }

    pub fn lookup(&self, handle: u32) -> Option<&Arc<Object>> {
        self.handles.get(&handle)
    }

    /// The object, for the caller to drop once the lock is gone: an object of the C++
    /// runs its destructor there.
    pub fn close(&mut self, handle: u32) -> Option<Arc<Object>> {
        self.handles.remove(&handle)
    }

    /// The object whose dispatcher header is at `address`, taken up from the header if
    /// it does not name one yet. None if the header's type is not a dispatcher object
    /// this kernel knows, or names one that is not a dispatcher.
    pub fn object_at(&mut self, memory: Memory, address: u32) -> Option<Arc<Object>> {
        if address == 0 {
            return None;
        }
        if memory.u32_at(address.wrapping_add(12)) == SIGNATURE
            && let Some(object) = self.headers.get(&memory.u32_at(address.wrapping_add(8)))
        {
            return object.is_dispatcher().then(|| Arc::clone(object));
        }
        let object = take_up(memory, address)?;
        self.attach(memory, &object, address);
        Some(object)
    }

    fn attach(&mut self, memory: Memory, object: &Arc<Object>, address: u32) {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        memory.store_u32(address.wrapping_add(8), id);
        memory.store_u32(address.wrapping_add(12), SIGNATURE);
        object.header.store(address, Relaxed);
        self.headers.insert(id, Arc::clone(object));
    }

    /// Gives `object` the header at `address`, writing its type there.
    pub fn attach_header(&mut self, memory: Memory, object: &Arc<Object>, address: u32, header_type: u8) {
        memory.store_u8(address, header_type);
        self.attach(memory, object, address);
    }
}

fn take_up(memory: Memory, address: u32) -> Option<Arc<Object>> {
    let state = memory.u32_at(address.wrapping_add(4)) as i32;
    let kind = match memory.u8_at(address) {
        header @ (NOTIFICATION_EVENT | SYNCHRONIZATION_EVENT) => {
            Kind::Event(Event::new(header == NOTIFICATION_EVENT, state != 0))
        }
        SEMAPHORE => Kind::Semaphore(Semaphore::new(state, memory.u32_at(address.wrapping_add(16)) as i32)),
        // Signal state 1 is free; a mutant initialised owned is not taken up here, since
        // the header does not say by which of our threads.
        MUTANT => Kind::Mutant(Mutant::new(false)),
        header @ (NOTIFICATION_TIMER | SYNCHRONIZATION_TIMER) => {
            Kind::Timer(Timer::new(header == NOTIFICATION_TIMER, state != 0))
        }
        _ => return None,
    };
    Some(Object::new(kind))
}

/// The object a header at `address` names; see Table::object_at.
pub fn object_at(memory: Memory, address: u32) -> Option<Arc<Object>> {
    lock().object_at(memory, address)
}

/// The object's dispatcher header, made in guest memory if it has none: what
/// ObReferenceObjectByHandle hands back.
pub fn header_of(memory: Memory, object: &Arc<Object>) -> u32 {
    {
        let _held = lock();
        let header = object.header.load(Relaxed);
        if header != 0 {
            return header;
        }
    }
    const HEADER_BYTES: u32 = 32;
    let address = allocate_guest(HEADER_BYTES, 16);
    if address == 0 {
        return 0;
    }
    memory.zero(address, HEADER_BYTES);
    lock().attach_header(memory, object, address, object.header_type());
    address
}

/// Signals an event (or a timer) and returns whether it was before.
pub fn set_event(object: &Object) -> bool {
    let held = lock();
    set_event_held(&held, object)
}

pub fn set_event_held(_held: &Held, object: &Object) -> bool {
    let Some(event) = object.as_event() else { return false };
    let was = event.signalled.swap(true, Relaxed);
    object.wake();
    was
}

/// Resets an event (or a timer) and returns whether it was signalled.
pub fn reset_event_held(_held: &Held, object: &Object) -> bool {
    object.as_event().is_some_and(|event| event.signalled.swap(false, Relaxed))
}

/// Adds to a semaphore's count, as far as its limit, and returns the count before.
pub fn release_semaphore(_held: &Held, object: &Object, delta: i32) -> i32 {
    let Kind::Semaphore(semaphore) = &object.kind else { return 0 };
    let previous = semaphore.count.load(Relaxed);
    let count = (i64::from(previous) + i64::from(delta)).min(i64::from(semaphore.limit));
    semaphore.count.store(count as i32, Relaxed);
    object.wake();
    previous
}

/// Releases a mutant the caller owns once, and returns how deep it was held before.
pub fn release_mutant(_held: &Held, object: &Object) -> i32 {
    let Kind::Mutant(mutant) = &object.kind else { return 0 };
    let previous = mutant.depth.load(Relaxed);
    if mutant.owner.load(Relaxed) == current_thread() && previous != 0 {
        mutant.depth.store(previous - 1, Relaxed);
        if previous == 1 {
            mutant.owner.store(0, Relaxed);
            object.wake();
        }
    }
    previous as i32
}

/// A thread has finished: what a wait on it waits for.
pub fn thread_exited(object: &Object) {
    let _held = lock();
    if let Some(thread) = object.as_thread() {
        thread.exited.store(true, Relaxed);
        object.wake();
    }
}

/// NT's timeout as a host deadline: negative is relative, positive an absolute system
/// time, and none or the most negative value never falls due.
pub fn deadline(timeout100ns: Option<i64>) -> Option<Instant> {
    let value = timeout100ns.filter(|&value| value != i64::MIN)?;
    let relative = if value < 0 { -value } else { value.wrapping_sub(system_time() as i64) };
    // A year is as good as for ever, and keeps the arithmetic in range.
    const YEAR: i64 = 365 * 24 * 3600 * 10_000_000;
    Some(Instant::now() + Duration::from_nanos(relative.clamp(0, YEAR) as u64 * 100))
}

/// The objects a wait names, kept alive for its length: no more than NT's
/// MAXIMUM_WAIT_OBJECTS, and nothing on the heap.
pub struct WaitList {
    objects: [Option<Arc<Object>>; MAXIMUM_WAIT_OBJECTS],
    count: usize,
}

pub const MAXIMUM_WAIT_OBJECTS: usize = 64;

impl WaitList {
    pub fn new() -> Self {
        Self { objects: [const { None }; MAXIMUM_WAIT_OBJECTS], count: 0 }
    }

    pub fn add(&mut self, object: &Arc<Object>) {
        self.objects[self.count] = Some(Arc::clone(object));
        self.count += 1;
    }

    fn iter(&self) -> impl Iterator<Item = &Object> {
        self.objects[..self.count].iter().flatten().map(|object| &**object)
    }
}

// One per thread, registered with every object it waits on. Woken once a sleep, however
// many of them are signalled: a futex wake is a system call, and one for a thread that is
// awake already is wasted where the lock is wanted most.
struct Waiter {
    condvar: Condvar,
    woken: AtomicBool, // under the dispatcher lock
}

impl Waiter {
    fn wake(&self) {
        if !self.woken.swap(true, Relaxed) {
            self.condvar.notify_one();
        }
    }
}

thread_local! {
    static WAITER: Arc<Waiter> = Arc::new(Waiter { condvar: Condvar::new(), woken: AtomicBool::new(false) });
}

/// STATUS_WAIT_0 + the index of the object that satisfied a wait for any of them,
/// STATUS_SUCCESS when a wait for all is satisfied, or STATUS_TIMEOUT.
pub fn wait(mut held: Held, list: &WaitList, all: bool, deadline: Option<Instant>) -> u32 {
    let me = current_thread();
    let mut waiter = None;
    let result = loop {
        if all {
            if list.iter().all(|object| object.signalled(me)) {
                list.iter().for_each(|object| object.satisfy(me));
                break STATUS_SUCCESS;
            }
        } else if let Some((index, object)) = list.iter().enumerate().find(|(_, object)| object.signalled(me)) {
            object.satisfy(me);
            break index as u32; // STATUS_WAIT_0 + index
        }
        let now = deadline.map(|deadline| (deadline, Instant::now()));
        if now.is_some_and(|(deadline, now)| now >= deadline) {
            break STATUS_TIMEOUT;
        }
        let waiter = waiter.get_or_insert_with(|| {
            let waiter = WAITER.with(Arc::clone);
            list.iter().for_each(|object| object.add_waiter(&waiter));
            waiter
        });
        waiter.woken.store(false, Relaxed);
        held = match now {
            Some((deadline, now)) => {
                waiter.condvar.wait_timeout(held, deadline - now).unwrap_or_else(PoisonError::into_inner).0
            }
            None => waiter.condvar.wait(held).unwrap_or_else(PoisonError::into_inner),
        };
    };
    if let Some(waiter) = waiter {
        list.iter().for_each(|object| object.remove_waiter(&waiter));
    }
    drop(held);
    result
}

/// Waits on one object until it is signalled.
pub fn wait_for(held: Held, object: &Arc<Object>) {
    let mut list = WaitList::new();
    list.add(object);
    wait(held, &list, false, None);
}

// Timers fall due on a thread of their own.
struct Due {
    timer: Weak<Object>,
    generation: u64,
    period_ms: u32,
}

struct TimerQueue {
    due: BTreeMap<(Instant, u64), Due>, // a later one at the same instant after
    added: u64,
    started: bool,
}

static TIMERS: Mutex<TimerQueue> = Mutex::new(TimerQueue { due: BTreeMap::new(), added: 0, started: false });
static TIMERS_CHANGED: Condvar = Condvar::new();

fn run_timers() {
    let mut queue = TIMERS.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        let Some((&(when, _), _)) = queue.due.first_key_value() else {
            queue = TIMERS_CHANGED.wait(queue).unwrap_or_else(PoisonError::into_inner);
            continue;
        };
        let now = Instant::now();
        if now < when {
            queue = TIMERS_CHANGED.wait_timeout(queue, when - now).unwrap_or_else(PoisonError::into_inner).0;
            continue;
        }
        let Some((_, due)) = queue.due.pop_first() else { continue };
        drop(queue);
        let mut again = false;
        if let Some(object) = due.timer.upgrade() {
            let _held = lock();
            if let Kind::Timer(timer) = &object.kind
                && timer.generation.load(Relaxed) == due.generation
            {
                timer.event.signalled.store(true, Relaxed);
                object.wake();
                again = due.period_ms != 0;
                timer.running.store(again, Relaxed);
            }
        }
        queue = TIMERS.lock().unwrap_or_else(PoisonError::into_inner);
        if again {
            let next = when + Duration::from_millis(u64::from(due.period_ms));
            add_due(&mut queue, next, due);
        }
    }
}

fn add_due(queue: &mut TimerQueue, when: Instant, due: Due) {
    queue.added += 1;
    let added = queue.added;
    queue.due.insert((when, added), due);
}

/// Schedules a timer: due after `due100ns` (NT's sign convention), then every
/// `period_ms` if that is not zero. Returns whether it was running.
pub fn set_timer(object: &Arc<Object>, due100ns: i64, period_ms: u32) -> bool {
    let Kind::Timer(timer) = &object.kind else { return false };
    let when = deadline(Some(due100ns)).unwrap_or_else(Instant::now);
    let (was, generation) = {
        let _held = lock();
        let was = timer.running.swap(true, Relaxed);
        timer.event.signalled.store(false, Relaxed);
        let generation = timer.generation.load(Relaxed) + 1;
        timer.generation.store(generation, Relaxed);
        (was, generation)
    };
    let mut queue = TIMERS.lock().unwrap_or_else(PoisonError::into_inner);
    if !queue.started {
        queue.started = true;
        std::thread::spawn(run_timers);
    }
    add_due(&mut queue, when, Due { timer: Arc::downgrade(object), generation, period_ms });
    TIMERS_CHANGED.notify_one();
    was
}

/// Stops a timer and returns whether it was running.
pub fn cancel_timer(_held: &Held, object: &Object) -> bool {
    let Kind::Timer(timer) = &object.kind else { return false };
    timer.generation.store(timer.generation.load(Relaxed) + 1, Relaxed);
    timer.running.swap(false, Relaxed)
}
