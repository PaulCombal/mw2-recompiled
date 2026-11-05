// Critical sections, spinlocks and thread-local storage.
//
// A critical section is the console's RTL_CRITICAL_SECTION, kept in guest memory as the
// kernel keeps it -- a dispatcher header, then the lock count (-1 when free), the
// recursion count and the owning KTHREAD -- and taken with an atomic on the lock count,
// as Xenia does. Only a thread that finds it held sleeps, on the header as a
// synchronization event.

use super::STATUS_SUCCESS;
use super::objects;
use crate::guest::{Memory, PPCContext, Register, kernel_import};
use std::cell::RefCell;
use std::sync::{Mutex, PoisonError};

const LOCK_COUNT: u32 = 16;
const RECURSION_COUNT: u32 = 20;
const OWNING_THREAD: u32 = 24;
const CURRENT_THREAD_OFFSET: u32 = 256; // KPCR.CurrentThread

fn current_kthread(ctx: &PPCContext, memory: Memory) -> u32 {
    memory.u32_at(ctx.r13.as_u32().wrapping_add(CURRENT_THREAD_OFFSET))
}

fn initialise(memory: Memory, cs: u32, spin_count: u32) {
    // A synchronization event, not signalled, its wait list empty.
    memory.store_u8(cs, 1);
    memory.store_u8(cs.wrapping_add(1), (spin_count.wrapping_add(255) / 256).min(255) as u8);
    memory.store_u32(cs.wrapping_add(4), 0);
    memory.store_u32(cs.wrapping_add(8), cs.wrapping_add(8));
    memory.store_u32(cs.wrapping_add(12), cs.wrapping_add(8));
    memory.store_u32(cs.wrapping_add(LOCK_COUNT), u32::MAX);
    memory.store_u32(cs.wrapping_add(RECURSION_COUNT), 0);
    memory.store_u32(cs.wrapping_add(OWNING_THREAD), 0);
}

fn take_if_owned(memory: Memory, cs: u32, me: u32) -> bool {
    if memory.u32_at(cs.wrapping_add(OWNING_THREAD)) != me {
        return false;
    }
    memory.add_i32(cs.wrapping_add(LOCK_COUNT), 1);
    let recursion = cs.wrapping_add(RECURSION_COUNT);
    memory.store_u32(recursion, memory.u32_at(recursion).wrapping_add(1));
    true
}

fn own(memory: Memory, cs: u32, me: u32) {
    memory.store_u32(cs.wrapping_add(OWNING_THREAD), me);
    memory.store_u32(cs.wrapping_add(RECURSION_COUNT), 1);
}

kernel_import!(RtlInitializeCriticalSection, |ctx, memory| {
    initialise(memory, ctx.r3.as_u32(), 0);
});

kernel_import!(RtlInitializeCriticalSectionAndSpinCount, |ctx, memory| {
    initialise(memory, ctx.r3.as_u32(), ctx.r4.as_u32());
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

kernel_import!(RtlEnterCriticalSection, |ctx, memory| {
    let cs = ctx.r3.as_u32();
    let me = current_kthread(ctx, memory);
    if take_if_owned(memory, cs, me) {
        return;
    }
    for _ in 0..u32::from(memory.u8_at(cs.wrapping_add(1))) * 256 {
        if memory.compare_exchange_u32(cs.wrapping_add(LOCK_COUNT), u32::MAX, 0) {
            own(memory, cs, me);
            return;
        }
    }
    // Counted in before sleeping, so the owner's leave knows to wake a waiter.
    if memory.add_i32(cs.wrapping_add(LOCK_COUNT), 1) != 0 {
        let mut held = objects::lock();
        if let Some(event) = held.object_at(memory, cs) {
            objects::wait_for(held, &event);
        }
    }
    own(memory, cs, me);
});

kernel_import!(RtlLeaveCriticalSection, |ctx, memory| {
    let cs = ctx.r3.as_u32();
    let recursion = cs.wrapping_add(RECURSION_COUNT);
    let depth = memory.u32_at(recursion).wrapping_sub(1);
    memory.store_u32(recursion, depth);
    if depth != 0 {
        memory.add_i32(cs.wrapping_add(LOCK_COUNT), -1);
        return;
    }
    memory.store_u32(cs.wrapping_add(OWNING_THREAD), 0);
    if memory.add_i32(cs.wrapping_add(LOCK_COUNT), -1) != -1 {
        let mut held = objects::lock();
        if let Some(event) = held.object_at(memory, cs) {
            objects::set_event_held(&held, &event);
        }
    }
});

kernel_import!(RtlTryEnterCriticalSection, |ctx, memory| {
    let cs = ctx.r3.as_u32();
    let me = current_kthread(ctx, memory);
    if memory.compare_exchange_u32(cs.wrapping_add(LOCK_COUNT), u32::MAX, 0) {
        own(memory, cs, me);
        ctx.r3.u64 = 1;
        return;
    }
    ctx.r3.u64 = take_if_owned(memory, cs, me).into();
});

// TLS slots. Allocation is process-wide, values are per-thread.
static TLS_USED: Mutex<Vec<bool>> = Mutex::new(Vec::new());

thread_local! {
    static TLS_VALUES: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

kernel_import!(KeTlsAlloc, |ctx, _memory| {
    let mut used = TLS_USED.lock().unwrap_or_else(PoisonError::into_inner);
    let slot = match used.iter().position(|&taken| !taken) {
        Some(free) => {
            used[free] = true;
            free
        }
        None => {
            used.push(true);
            used.len() - 1
        }
    };
    ctx.r3.u64 = u64::from(slot as u32);
});

kernel_import!(KeTlsFree, |ctx, _memory| {
    let mut used = TLS_USED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(slot) = used.get_mut(ctx.r3.as_u32() as usize) {
        *slot = false;
    }
    ctx.r3.u64 = 1;
});

kernel_import!(KeTlsGetValue, |ctx, _memory| {
    let slot = ctx.r3.as_u32() as usize;
    ctx.r3.u64 = u64::from(TLS_VALUES.with_borrow(|values| values.get(slot).copied().unwrap_or(0)));
});

kernel_import!(KeTlsSetValue, |ctx, _memory| {
    let slot = ctx.r3.as_u32() as usize;
    let value = ctx.r4.as_u32();
    TLS_VALUES.with_borrow_mut(|values| {
        if slot >= values.len() {
            values.resize(slot + 1, 0);
        }
        values[slot] = value;
    });
    ctx.r3.u64 = 1;
});

// Critical regions and IRQL mean nothing under a host scheduler. A spinlock is a word in
// guest memory holding its owner's r13, as Xenia keeps it.
kernel_import!(KeEnterCriticalRegion, |_ctx, _memory| {});

kernel_import!(KeLeaveCriticalRegion, |_ctx, _memory| {});

kernel_import!(KeRaiseIrqlToDpcLevel, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

kernel_import!(KfLowerIrql, |_ctx, _memory| {});

fn acquire_spin_lock(memory: Memory, lock: u32, owner: u32) {
    while !memory.compare_exchange_u32(lock, 0, owner) {
        std::thread::yield_now();
    }
}

kernel_import!(KfAcquireSpinLock, |ctx, memory| {
    acquire_spin_lock(memory, ctx.r3.as_u32(), ctx.r13.as_u32());
    ctx.r3.u64 = 0; // the previous IRQL
});

kernel_import!(KfReleaseSpinLock, |ctx, memory| {
    memory.store_atomic_u32(ctx.r3.as_u32(), 0);
});

kernel_import!(KeAcquireSpinLockAtRaisedIrql, |ctx, memory| {
    acquire_spin_lock(memory, ctx.r3.as_u32(), ctx.r13.as_u32());
});

kernel_import!(KeReleaseSpinLockFromRaisedIrql, |ctx, memory| {
    memory.store_atomic_u32(ctx.r3.as_u32(), 0);
});

kernel_import!(KeTryToAcquireSpinLockAtRaisedIrql, |ctx, memory| {
    ctx.r3.u64 = memory.compare_exchange_u32(ctx.r3.as_u32(), 0, ctx.r13.as_u32()).into();
});
