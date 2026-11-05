// Guest threads. Each gets a host thread, its own guest stack, its own KPCR block for
// r13, and its own PPCContext.

use super::objects::{self, Event, Kind, Object, THREAD};
use super::{STATUS_INVALID_PARAMETER, STATUS_NO_MEMORY, STATUS_SUCCESS, allocate_guest, create_thread_block};
use crate::guest::{self, Memory, Register, kernel_import};
use crate::log::{format_text, log_error, log_kernel, log_warn};
use crate::runtime::{crash, guest_memory, platform};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

const DEFAULT_STACK_SIZE: u32 = 512 * 1024;
const CURRENT_THREAD_OFFSET: u32 = 256; // KPCR.CurrentThread
const PROCESSOR_OFFSET: u32 = 268; // KPCR.CurrentProcessorNumber

// The main thread is id 1 (run.rs); the first thread the title creates is XAudio2's
// mixer, and game code that runs from its voice callbacks must not pass the title's "is
// this the main thread" checks.
static NEXT_THREAD_ID: AtomicU32 = AtomicU32::new(2);

/// A fresh id from the same sequence guest threads draw on, for a host thread that runs
/// guest code (the audio worker).
pub fn new_thread_id() -> u32 {
    NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed)
}

/// Signalled once it has exited, which is what a wait on a thread waits for.
pub struct GuestThread {
    thread_id: u32,
    xapi_startup: u32,
    start_address: u32,
    start_context: u32,
    stack_base: u32,
    stack_size: u32, // all of it the thread's: a larger one than the default is not cut short
    pcr: u32,
    processor: AtomicU8,
    pub(super) exited: AtomicBool, // under the dispatcher lock
    resume_gate: Arc<Object>,      // signalled when the thread may run
}

impl GuestThread {
    fn resume(&self) {
        objects::set_event(&self.resume_gate);
    }

    // Says which hardware thread it is on, where the title reads it.
    fn set_processor(&self, memory: Memory, processor: u8) {
        self.processor.store(processor, Ordering::Relaxed);
        memory.store_u8(self.pcr.wrapping_add(PROCESSOR_OFFSET), processor);
    }
}

// The host thread holds the object for as long as it runs, across the guest call: a
// longjmp can only go back to a setjmp of this thread's own guest code, which is all
// below this frame.
fn body(object: Arc<Object>) {
    let Some(thread) = object.as_thread() else { return };
    objects::wait_for(objects::lock(), &thread.resume_gate);

    // ExCreateThread's fourth argument is XAPI's thread-startup shim, and the kernel
    // enters that rather than the routine: it installs the per-thread state -- notably
    // the TLS array the KPCR points at, out of which the CRT and zlib allocate -- and
    // then calls the routine. Jumping straight to the routine leaves every allocation
    // from a created thread reading an empty pool.
    let via_shim = thread.xapi_startup != 0;
    let entry = if via_shim { thread.xapi_startup } else { thread.start_address };
    let memory = guest_memory::memory();
    let Some(function) = memory.function_at(entry) else {
        log_error!("thread %u: no recompiled function at %08X", thread.thread_id, entry);
        objects::thread_exited(&object);
        return;
    };

    let mut ctx = guest::blank_context();
    ctx.r1.u64 = u64::from(thread.stack_base.wrapping_add(thread.stack_size).wrapping_sub(0x100) & !0xF);
    ctx.r13.u64 = u64::from(thread.pcr);
    if via_shim {
        ctx.r3.u64 = u64::from(thread.start_address);
        ctx.r4.u64 = u64::from(thread.start_context);
    } else {
        ctx.r3.u64 = u64::from(thread.start_context);
    }
    guest::load_float_mode(&mut ctx);

    crash::register_thread(&format_text!("guest thread %u (routine %08X)", thread.thread_id, thread.start_address));

    function.call(&mut ctx, memory);
    log_kernel!("thread %u returned", thread.thread_id);
    crash::unregister_thread();
    objects::thread_exited(&object);
}

// What a C++ std::thread gets, which is what the recompiled code was measured on: it nests
// deep on the host stack.
pub(super) fn host_stack_size() -> usize {
    static SIZE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *SIZE.get_or_init(platform::default_thread_stack)
}

fn launch(object: Arc<Object>) {
    std::thread::Builder::new()
        .stack_size(host_stack_size())
        .spawn(move || body(object))
        .expect("a host thread for a guest thread");
}

kernel_import!(ExCreateThread, |ctx, memory| {
    let handle_out = ctx.r3.as_u32();
    let stack_size = if ctx.r4.as_u32() != 0 { ctx.r4.as_u32() } else { DEFAULT_STACK_SIZE };
    let thread_id_out = ctx.r5.as_u32();
    let xapi_startup = ctx.r6.as_u32();
    let start_address = ctx.r7.as_u32();
    let start_context = ctx.r8.as_u32();
    let creation_flags = ctx.r9.as_u32();

    if !(ppc_rt::PPC_CODE_BASE..ppc_rt::PPC_CODE_BASE + ppc_rt::PPC_CODE_SIZE).contains(&u64::from(start_address)) {
        log_warn!("ExCreateThread: start address %08X is not guest code", start_address);
        ctx.r3.u64 = u64::from(STATUS_INVALID_PARAMETER);
        return;
    }

    let thread_id = new_thread_id();
    let stack_bytes = stack_size.max(DEFAULT_STACK_SIZE);
    let stack_base = allocate_guest(stack_bytes, 0x1000);
    let pcr = create_thread_block(thread_id);
    if stack_base == 0 || pcr == 0 {
        ctx.r3.u64 = u64::from(STATUS_NO_MEMORY);
        return;
    }

    let object = Object::new(Kind::Thread(GuestThread {
        thread_id,
        xapi_startup,
        start_address,
        start_context,
        stack_base,
        stack_size: stack_bytes,
        pcr,
        processor: AtomicU8::new(0),
        exited: AtomicBool::new(false),
        resume_gate: Object::new(Kind::Event(Event::new(true, false))),
    }));
    let Some(thread) = object.as_thread() else { return };

    // The top byte of the flags is a mask naming the hardware thread to start on (one
    // bit; Xenia's GetFakeCpuNumber). XAudio2 creates one mixing thread per processor it
    // was given and each waits, by processor number, for the others to check in -- with
    // every thread reporting 0 that barrier never completes and the mixer stalls after
    // its first frame.
    let mask = creation_flags >> 24;
    if mask != 0 {
        thread.set_processor(memory, mask.trailing_zeros() as u8);
    }

    let handle = {
        let mut held = objects::lock();
        let handle = held.insert(Arc::clone(&object));
        // The KTHREAD is the thread's dispatcher header: ObReferenceObjectByHandle hands
        // it out and KeResumeThread and KeSetAffinityThread take it.
        let kthread = memory.u32_at(pcr.wrapping_add(CURRENT_THREAD_OFFSET));
        held.attach_header(memory, &object, kthread, THREAD);
        handle
    };
    if handle_out != 0 {
        memory.store_u32(handle_out, handle);
    }
    if thread_id_out != 0 {
        memory.store_u32(thread_id_out, thread_id);
    }

    launch(Arc::clone(&object));
    const CREATE_SUSPENDED: u32 = 1;
    if creation_flags & CREATE_SUSPENDED == 0 {
        thread.resume();
    }

    log_kernel!(
        "ExCreateThread -> id %u handle %08X at %08X, %u KB stack at %08X, processor %u (flags %08X), r13 %08X%s",
        thread_id,
        handle,
        start_address,
        stack_size / 1024,
        stack_base,
        u32::from(thread.processor.load(Ordering::Relaxed)),
        creation_flags,
        pcr,
        if creation_flags & CREATE_SUSPENDED != 0 { c" (suspended)" } else { c"" }
    );
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

kernel_import!(NtResumeThread, |ctx, memory| {
    let object = objects::lock().lookup(ctx.r3.as_u32()).cloned();
    if ctx.r4.as_u32() != 0 {
        memory.store_u32(ctx.r4.as_u32(), 1);
    }
    if let Some(thread) = object.as_deref().and_then(Object::as_thread) {
        thread.resume();
    }
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

// Takes the thread object (its KTHREAD) rather than a handle. XAudio2 creates its mixing
// thread suspended and starts it this way.
kernel_import!(KeResumeThread, |ctx, memory| {
    let object = objects::object_at(memory, ctx.r3.as_u32());
    match object.as_deref().and_then(Object::as_thread) {
        Some(thread) => thread.resume(),
        None => log_warn!("KeResumeThread: %08X is not a guest thread", ctx.r3.as_u32()),
    }
    ctx.r3.u64 = 0;
});

kernel_import!(NtSuspendThread, |ctx, memory| {
    // Suspending a running host thread safely is not possible here; report success so the
    // title's bookkeeping stays consistent.
    if ctx.r4.as_u32() != 0 {
        memory.store_u32(ctx.r4.as_u32(), 0);
    }
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

kernel_import!(ExTerminateThread, |ctx, _memory| {
    log_kernel!("ExTerminateThread(%u)", ctx.r3.as_u32());
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

// XSetThreadProcessor reaches the kernel as a one-bit affinity mask over the console's
// six hardware threads. The number behind it is not bookkeeping: IW4 reads the processor
// out of its own KPCR (a byte at r13+268) and dispatches on it. The renderer's
// command-buffer jobs are only ever accepted on processors 1 and 2, so with every thread
// reporting 0 they are refused for ever and the first frame of a level never finishes.
kernel_import!(KeSetAffinityThread, |ctx, memory| {
    let affinity = ctx.r4.as_u32();
    let object = objects::object_at(memory, ctx.r3.as_u32());
    match object.as_deref().and_then(Object::as_thread) {
        Some(thread) if affinity != 0 => thread.set_processor(memory, affinity.trailing_zeros() as u8),
        None if affinity != 0 => log_warn!("KeSetAffinityThread: %08X is not a guest thread", ctx.r3.as_u32()),
        _ => {}
    }
    ctx.r3.u64 = u64::from(affinity);
});

// MW2 asks for 15 for one thread at start and 16 for short-lived ones, and drops what
// the call returns. Xenia gives every priority from 10 to 16 the host's normal one, so
// leaving the host thread as it is does what it does.
kernel_import!(KeSetBasePriorityThread, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

kernel_import!(ExRegisterTitleTerminateNotification, |ctx, _memory| {
    ctx.r3.u64 = 0;
});
