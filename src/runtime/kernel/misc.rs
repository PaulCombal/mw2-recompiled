// The clocks, delays and small queries the title makes of the kernel.

use super::{
    STATUS_INVALID_PARAMETER, STATUS_NO_MEMORY, STATUS_SUCCESS, STATUS_TIMEOUT, STATUS_USER_APC, deliver_user_apcs,
    report_unimplemented,
};
use crate::guest::{Memory, Register, kernel_import};
use crate::log::{log_info, log_kernel, log_warn};
use crate::runtime::{bridge, install};
use std::ffi::CString;
use std::sync::LazyLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const STATUS_NO_SUCH_FILE: u32 = 0xC000_000F;
pub const STATUS_END_OF_FILE: u32 = 0xC000_0011;
pub const STATUS_OBJECT_NAME_NOT_FOUND: u32 = 0xC000_0034;
const STATUS_DLL_NOT_FOUND: u32 = 0xC000_0135;
const STATUS_ENTRYPOINT_NOT_FOUND: u32 = 0xC000_0139;
pub const ERROR_SUCCESS: u32 = 0;
pub const ERROR_NOT_FOUND: u32 = 0x0490;

/// 100ns ticks since 1601-01-01, the epoch the console reports.
#[cfg(not(mw2_rig))]
pub fn system_time() -> u64 {
    // From 1970 on, as the C++'s system_clock counted.
    let since_1970 = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_nanos() as i64);
    ((since_1970 / 100) as u64).wrapping_add(116_444_736_000_000_000)
}

// A rig's fixed time, so that absolute timeouts fall due the same on both of its sides.
#[cfg(mw2_rig)]
#[allow(unsafe_code)]
pub fn system_time() -> u64 {
    unsafe extern "C" {
        fn mw2_kernel_system_time() -> u64;
    }
    unsafe { mw2_kernel_system_time() }
}

// The guest timebase, which every mftb in the recompiled code reads. It runs at the
// console's 50 MHz, not at the host's cycle counter: the title only sometimes asks
// KeQueryPerformanceFrequency, and MW2's server thread times its frames with a constant
// 1/50000 ms per tick -- on a faster counter the game ran at the frame rate instead of
// its own clock.
const TIMEBASE_HZ: u64 = 50_000_000;
static TIMEBASE_START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Starts the timebase, before the title can read it.
pub fn start_timebase() {
    LazyLock::force(&TIMEBASE_START);
}

pub fn timebase() -> u64 {
    (TIMEBASE_START.elapsed().as_nanos() as u64) / (1_000_000_000 / TIMEBASE_HZ)
}

kernel_import!(KeGetCurrentProcessType, |ctx, _memory| {
    ctx.r3.u64 = 1; // title
});

kernel_import!(XexCheckExecutablePrivilege, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

kernel_import!(HalReturnToFirmware, |ctx, _memory| {
    log_info!("HalReturnToFirmware(%u) -- guest asked to shut down", ctx.r3.as_u32());
    report_unimplemented();
    std::process::exit(0);
});

kernel_import!(KeQueryPerformanceFrequency, |ctx, _memory| {
    ctx.r3.u64 = TIMEBASE_HZ;
});

kernel_import!(KeQuerySystemTime, |ctx, memory| {
    if ctx.r3.as_u32() != 0 {
        memory.store_u64(ctx.r3.as_u32(), system_time());
    }
});

// (processorMode, alertable, intervalPtr) -- 100ns, negative = relative
kernel_import!(KeDelayExecutionThread, |ctx, memory| {
    if ctx.r4.as_u32() != 0 && deliver_user_apcs(ctx, memory) {
        ctx.r3.u64 = u64::from(STATUS_USER_APC);
        return;
    }
    let interval = ctx.r5.as_u32();
    if interval != 0 {
        let value = memory.u64_at(interval) as i64;
        // The most negative value is "infinite": the caller expects an APC or an alert.
        if value == i64::MIN {
            log_warn!("KeDelayExecutionThread: an infinite wait; the caller expects an APC or an alert");
            std::thread::sleep(Duration::from_millis(1));
        } else if value < 0 {
            let nanoseconds = value.wrapping_neg().wrapping_mul(100);
            if nanoseconds > 1_000_000_000 {
                log_warn!("KeDelayExecutionThread: sleeping %.1f seconds", nanoseconds as f64 / 1e9);
            }
            super::sync_objects::note(SLEEP, ctx.lr as u32);
            super::sync_objects::note_callers(ctx.r1.as_u32());
            std::thread::sleep(Duration::from_nanos(nanoseconds.max(0) as u64));
            super::sync_objects::note(super::sync_objects::KERNEL_WAIT_END, 0);
        } else {
            std::thread::yield_now();
        }
    }
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

// The pacing trace's mark for a thread that sleeps, called from its value.
const SLEEP: u8 = b'D';

// The campaign and the multiplayer are two executables, and the title goes from one to
// the other by asking the loader for it. What it wants the other to know -- which menu
// to open, who was signed in -- it leaves as launch data first, and reads back at its
// own start (runtime/install/).
// (buffer, size)
kernel_import!(XamLoaderGetLaunchData, |ctx, memory| {
    let data = install::launch_data();
    if data.is_empty() {
        ctx.r3.u64 = u64::from(ERROR_NOT_FOUND);
        return;
    }
    memory.store_bytes(ctx.r3.as_u32(), &data[..data.len().min(ctx.r4.as_u32() as usize)]);
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

// (data, size)
kernel_import!(XamLoaderSetLaunchData, |ctx, memory| {
    let data = if ctx.r3.as_u32() != 0 { memory.bytes(ctx.r3.as_u32(), ctx.r4.as_u32() as usize) } else { Vec::new() };
    install::set_launch_data(&data);
    ctx.r3.u64 = u64::from(ERROR_SUCCESS);
});

// Neither call returns on the console: the title is gone when it would.
fn end_title(why: &std::ffi::CStr) -> ! {
    bridge::request_exit(why);
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

// (path, flags). No path is the way back to the dashboard.
kernel_import!(XamLoaderLaunchTitle, |ctx, memory| {
    let path = ctx.r3.as_u32();
    if path != 0 {
        let full = memory.string(path, usize::MAX);
        let start = full.iter().rposition(|&byte| matches!(byte, b'\\' | b'/' | b':')).map_or(0, |slash| slash + 1);
        let name = CString::new(full[start..].to_ascii_lowercase()).unwrap_or_default();
        log_info!("XamLoaderLaunchTitle: %s (flags %X)", memory.text(path), ctx.r4.as_u32());
        install::set_next_title(&name);
        end_title(c"the title started its other executable");
    }
    end_title(c"the title left for the dashboard");
});

kernel_import!(XamLoaderTerminateTitle, |_ctx, _memory| {
    end_title(c"the title ended itself");
});

// The lowest version with both XSessionMigrateHost and XSessionModifySkill, which the
// multiplayer does not send on an older one. Newer ones add nothing the title can use:
// it looks for one more entry point in xam.xex at 0x20211000.
kernel_import!(XamGetSystemVersion, |ctx, _memory| {
    ctx.r3.u64 = 0x200E_D800;
});

// No system module is there to load or look into. The title's own wrappers take "not
// found" as the answer to fall back from; a success that wrote no handle sent them into
// whatever the stack held.
fn store_zero(memory: Memory, address: u32) {
    if address != 0 {
        memory.store_u32(address, 0);
    }
}

// (name, handleOut)
kernel_import!(XexGetModuleHandle, |ctx, memory| {
    store_zero(memory, ctx.r4.as_u32());
    ctx.r3.u64 = u64::from(STATUS_DLL_NOT_FOUND);
});

// (module, ordinal, addressOut)
kernel_import!(XexGetProcedureAddress, |ctx, memory| {
    store_zero(memory, ctx.r5.as_u32());
    ctx.r3.u64 = u64::from(STATUS_ENTRYPOINT_NOT_FOUND);
});

// (name, flags, minimumVersion, handleOut)
kernel_import!(XexLoadImage, |ctx, memory| {
    store_zero(memory, ctx.r6.as_u32());
    ctx.r3.u64 = u64::from(STATUS_DLL_NOT_FOUND);
});

kernel_import!(XexUnloadImage, |ctx, _memory| {
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

kernel_import!(XGetAVPack, |ctx, _memory| {
    ctx.r3.u64 = 0x01_0000; // HDMI
});

kernel_import!(XGetLanguage, |ctx, _memory| {
    ctx.r3.u64 = 1; // English
});

kernel_import!(KeLockL2, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

kernel_import!(KeUnlockL2, |_ctx, _memory| {});

kernel_import!(KiApcNormalRoutineNop, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

kernel_import!(DbgPrint, |ctx, memory| {
    // Guest varargs are awkward and the format string alone is the useful part.
    let mut format = if ctx.r3.as_u32() != 0 { memory.string(ctx.r3.as_u32(), usize::MAX) } else { Vec::new() };
    if format.last() == Some(&b'\n') {
        format.pop();
    }
    log_kernel!("DbgPrint: %s", CString::new(format).unwrap_or_default().as_c_str());
});

// NTSTATUS -> Win32 error. Called on every failed operation, so only the codes the
// runtime actually returns matter.
kernel_import!(RtlNtStatusToDosError, |ctx, _memory| {
    ctx.r3.u64 = match ctx.r3.as_u32() {
        STATUS_SUCCESS => 0,                                     // ERROR_SUCCESS
        STATUS_OBJECT_NAME_NOT_FOUND | STATUS_NO_SUCH_FILE => 2, // ERROR_FILE_NOT_FOUND
        STATUS_END_OF_FILE => 38,                                // ERROR_HANDLE_EOF
        STATUS_INVALID_PARAMETER => 87,                          // ERROR_INVALID_PARAMETER
        STATUS_NO_MEMORY => 8,                                   // ERROR_NOT_ENOUGH_MEMORY
        STATUS_TIMEOUT => 1460,                                  // ERROR_TIMEOUT
        _ => 317,                                                // ERROR_MR_MID_NOT_FOUND
    };
});

// Without this every OBJECT_ATTRIBUTES the title builds carries an empty name, so no
// file can ever be opened. ANSI_STRING: length, maximum length, pointer.
kernel_import!(RtlInitAnsiString, |ctx, memory| {
    let destination = ctx.r3.as_u32();
    if destination == 0 {
        return;
    }
    let source = ctx.r4.as_u32();
    let length = if source != 0 { memory.string(source, usize::MAX).len() } else { 0 };
    memory.store_u16(destination, length as u16);
    memory.store_u16(destination + 2, if source != 0 { (length + 1) as u16 } else { 0 });
    memory.store_u32(destination + 4, source);
});

// The kernel's code page is Latin-1 as far as the title's text goes: each byte is one
// UTF-16 unit. Both take byte counts and write back how many bytes they produced. The
// title builds Live storage file names with the first.
//
// (unicodeOut, maxBytesOut, bytesOut, multiByte, bytesIn)
kernel_import!(RtlMultiByteToUnicodeN, |ctx, memory| {
    let (out, input) = (ctx.r3.as_u32(), ctx.r6.as_u32());
    let count = if out != 0 && input != 0 { (ctx.r4.as_u32() / 2).min(ctx.r7.as_u32()) } else { 0 };
    for i in 0..count {
        memory.store_u16(out.wrapping_add(i * 2), u16::from(memory.u8_at(input.wrapping_add(i))));
    }
    if ctx.r5.as_u32() != 0 {
        memory.store_u32(ctx.r5.as_u32(), count.wrapping_mul(2));
    }
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

// (multiByteOut, maxBytesOut, bytesOut, unicode, bytesIn)
kernel_import!(RtlUnicodeToMultiByteN, |ctx, memory| {
    let (out, input) = (ctx.r3.as_u32(), ctx.r6.as_u32());
    let count = if out != 0 && input != 0 { ctx.r4.as_u32().min(ctx.r7.as_u32() / 2) } else { 0 };
    for i in 0..count {
        let unit = memory.u16_at(input.wrapping_add(i * 2));
        memory.store_u8(out.wrapping_add(i), if unit < 0x100 { unit as u8 } else { b'?' });
    }
    if ctx.r5.as_u32() != 0 {
        memory.store_u32(ctx.r5.as_u32(), count);
    }
    ctx.r3.u64 = u64::from(STATUS_SUCCESS);
});

kernel_import!(RtlFillMemoryUlong, |ctx, memory| {
    let destination = ctx.r3.as_u32();
    if destination == 0 {
        return;
    }
    for i in 0..(ctx.r4.as_u32() & !3) / 4 {
        memory.store_u32(destination.wrapping_add(i * 4), ctx.r5.as_u32());
    }
});

kernel_import!(RtlCompareMemoryUlong, |ctx, memory| {
    let (source, length) = (ctx.r3.as_u32(), ctx.r4.as_u32() & !3);
    let mut matched = 0;
    if source != 0 {
        while matched < length && memory.u32_at(source.wrapping_add(matched)) == ctx.r5.as_u32() {
            matched += 4;
        }
    }
    ctx.r3.u64 = u64::from(matched);
});
