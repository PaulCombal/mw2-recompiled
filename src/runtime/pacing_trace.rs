// MW2_TRACE_PACING=<file>: see runtime/pacing_trace.h.

use crate::runtime::clock;

/// Microseconds since the start, on the clock the trace is written in.
pub fn microseconds(now: u64) -> i64 {
    (now.wrapping_sub(clock::ORIGIN) / 1000) as i64
}

#[cfg(feature = "diagnostics")]
pub use trace::*;

#[cfg(feature = "diagnostics")]
mod trace {
    use super::microseconds;
    use crate::env;
    use crate::log::log_info;
    use crate::runtime::guest_memory;
    use crate::runtime::platform;
    use std::ffi::CStr;
    #[cfg(not(windows))]
    use std::fs;
    use std::fs::File;
    use std::io::{BufWriter, Write};
    use std::sync::LazyLock;
    use std::sync::atomic::{AtomicI64, AtomicU8, AtomicU32, AtomicUsize, Ordering};

    const ENTRIES: usize = 16 << 20;
    pub const CALLER: u8 = b'X';

    // Filled by many threads with no lock, and read when the run is over.
    #[derive(Default)]
    struct Entry {
        at: AtomicI64,
        value: AtomicU32,
        thread: AtomicU32,
        event: AtomicU8,
    }

    static TABLE: LazyLock<Box<[Entry]>> = LazyLock::new(|| {
        let mut table = Vec::new();
        table.resize_with(ENTRIES, Entry::default);
        table.into_boxed_slice()
    });
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    pub fn path() -> Option<&'static CStr> {
        env::text(c"MW2_TRACE_PACING")
    }

    /// Whether the trace is wanted, read once.
    pub fn on() -> bool {
        static ON: LazyLock<bool> = LazyLock::new(|| path().is_some());
        *ON
    }

    pub fn note(now: u64, event: u8, value: u32, thread: u32) {
        // tools/hook_diff.py: which notes a hook made, and in what order with its other
        // calls, whichever side's they were; a thread's CPU time ('C') is the clock's, so
        // its value is not.
        #[cfg(mw2_hook_rig)]
        log_info!("[pacing] %u %u", u32::from(event), if event == b'C' { 0 } else { value });
        let entries = &**TABLE;
        let at = NEXT.fetch_add(1, Ordering::Relaxed);
        let Some(entry) = entries.get(at) else { return };
        entry.at.store(microseconds(now), Ordering::Relaxed);
        entry.value.store(value, Ordering::Relaxed);
        entry.thread.store(thread, Ordering::Relaxed);
        entry.event.store(event, Ordering::Relaxed);
    }

    // Each frame's back chain is at 0(r1); a function saves its return address at -8
    // from its caller's stack pointer.
    pub fn note_callers(mut note: impl FnMut(u32), r1: u32) {
        let memory = guest_memory::memory();
        let mut frame = r1;
        for _ in 0..3 {
            if frame == 0 {
                break;
            }
            let caller = memory.u32_at(frame);
            if caller == 0 {
                break;
            }
            note(memory.u32_at(caller.wrapping_sub(8)));
            frame = caller;
        }
    }

    // The threads by name, for the thread column.
    #[cfg(not(windows))]
    fn names(file: &mut impl Write) -> std::io::Result<()> {
        let Ok(tasks) = fs::read_dir("/proc/self/task") else { return Ok(()) };
        for task in tasks.flatten() {
            let id = task.file_name();
            let comm = fs::read(task.path().join("comm")).unwrap_or_default();
            let name = comm.split(|&byte| byte == b'\n').next().unwrap_or_default();
            writeln!(file, "# {} {}", id.to_string_lossy(), String::from_utf8_lossy(name))?;
        }
        Ok(())
    }

    #[cfg(windows)]
    fn names(_file: &mut impl Write) -> std::io::Result<()> {
        Ok(())
    }

    pub fn write(path: &CStr) {
        let Ok(file) = File::create(platform::path(path)) else { return };
        let mut file = BufWriter::new(file);
        let count = NEXT.load(Ordering::SeqCst).min(ENTRIES);
        let written = names(&mut file).and_then(|()| {
            // A table never noted in is never made.
            for entry in if count == 0 { &[] } else { &TABLE[..count] } {
                writeln!(
                    file,
                    "{} {} {} {}",
                    entry.at.load(Ordering::Relaxed),
                    entry.event.load(Ordering::Relaxed) as char,
                    entry.value.load(Ordering::Relaxed),
                    entry.thread.load(Ordering::Relaxed)
                )?;
            }
            file.flush()
        });
        // What fopen and fprintf leave of a full disk is as much of it as went in.
        let _ = written;
        log_info!("pacing: %llu events written to %s", count as u64, path);
    }
}
