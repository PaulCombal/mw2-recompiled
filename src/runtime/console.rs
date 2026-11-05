// Feeds text into IW4's own console command buffer: the shortest path to a level, or
// to any setting, is to queue the command the menu would have queued.
//
//   MW2_CONSOLE="30:map af_caves"          one command, at 30 seconds
//   MW2_CONSOLE="30:map af_caves;45:god"   several, separated by ';'
//
// The time is wall-clock seconds since the runtime started, the same clock
// MW2_INPUT_SCRIPT uses, so the two can be interleaved.
//
// Cbuf_AddText copies the text, so the string only has to survive the call. It goes
// just below the guest stack pointer of whichever thread is pumping -- memory below
// r1 belongs to nobody -- with the call made at a stack pointer pushed past it, so
// the callee's own frame cannot reach it. Nothing owned is held over that call,
// which a longjmp could skip: the line is on this stack by then.

use crate::env;
use crate::guest::{Memory, PPCContext, Register, preserving};
use crate::log::{log_info, log_warn};
use crate::title::*;
use std::ffi::{CStr, CString};
use std::sync::Mutex;
use std::time::Instant;

// With its newline.
const LONGEST: usize = 192;

struct Entry {
    at: f64,
    text: CString,
    done: bool,
}

struct Script {
    entries: Vec<Entry>,
    // place(), the latest only.
    place: Option<CString>,
    start: Option<Instant>,
}

static SCRIPT: Mutex<Script> = Mutex::new(Script { entries: Vec::new(), place: None, start: None });

impl Script {
    // A command may contain spaces; only ';' separates entries and only the first ':'
    // separates the time.
    fn parse(&mut self) {
        self.start = Some(Instant::now());
        let Some(spec) = env::text(c"MW2_CONSOLE") else { return };
        for item in spec.to_bytes().split(|&byte| byte == b';') {
            let Some(colon) = item.iter().position(|&byte| byte == b':').filter(|&colon| colon != 0) else { continue };
            let text = &item[colon + 1..];
            if text.is_empty() {
                continue;
            }
            // Out of a C string, so neither holds a zero.
            let (Ok(at), Ok(text)) = (CString::new(&item[..colon]), CString::new(text)) else { continue };
            self.entries.push(Entry { at: env::parse_real(&at), text, done: false });
        }
        for entry in &self.entries {
            log_info!("console: will run \"%s\" at %.2f s", entry.text.as_c_str(), entry.at);
        }
    }

    fn elapsed(&self) -> f64 {
        self.start.map_or(0.0, |start| start.elapsed().as_secs_f64())
    }

    fn parsed(&mut self) -> &mut Self {
        if self.start.is_none() {
            self.parse();
        }
        self
    }
}

fn script() -> std::sync::MutexGuard<'static, Script> {
    SCRIPT.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Queues one command for the next pump, from any thread -- a key press.
pub fn run_now(text: &CStr) {
    script().parsed().entries.push(Entry { at: 0.0, text: text.to_owned(), done: false });
}

/// The same for a command sent every frame, a camera being moved: the one waiting is
/// replaced instead of queued behind, and none of them is logged.
pub fn place(text: &CStr) {
    script().place = Some(text.to_owned()).filter(|text| !text.is_empty());
}

// The next command due, and whether it is a placed one.
fn next() -> Option<(CString, bool)> {
    let mut script = script();
    let script = script.parsed();
    if script.entries.is_empty() && script.place.is_none() {
        return None;
    }
    let now = script.elapsed();
    // One per pump; they queue anyway.
    if let Some(entry) = script.entries.iter_mut().find(|entry| !entry.done && now >= entry.at) {
        entry.done = true;
        if !entry.text.is_empty() {
            return Some((entry.text.clone(), false));
        }
    }
    script.place.take().map(|text| (text, true))
}

/// Queues commands from MW2_CONSOLE into the title's command buffer. Call from a hook
/// that runs on a guest thread every frame; it does nothing until one of the scripted
/// times has passed, and nothing at all without MW2_CONSOLE.
pub fn pump(ctx: &mut PPCContext, memory: Memory) {
    let Some((text, placing)) = next() else { return };

    // Cbuf_AddText wants a newline-terminated line, the way a .cfg supplies it.
    let mut line = [0u8; LONGEST + 2];
    let bytes = text.as_bytes();
    let newline = usize::from(bytes.last() != Some(&b'\n'));
    let size = bytes.len() + newline;
    if size > LONGEST {
        log_warn!("console: \"%s\" is too long", text.as_c_str());
        return;
    }
    line[..bytes.len()].copy_from_slice(bytes);
    line[size - 1] = b'\n';
    drop(text);

    let sp = ctx.r1.as_u32();
    if sp < 0x1000 {
        log_warn!("console: no guest stack to borrow");
        return;
    }

    let scratch = sp.wrapping_sub(0x100) & !0xF;
    memory.store_bytes(scratch, &line[..=size]);

    preserving(ctx, |ctx| {
        ctx.r1.u64 = u64::from(sp.wrapping_sub(0x200) & !0xF);
        ctx.r3.u64 = 0; // localClientNum
        ctx.r4.u64 = u64::from(scratch);
        T_Cbuf_AddText.call(ctx, memory);
    });

    if !placing {
        line[size - 1] = 0;
        if let Ok(shown) = CStr::from_bytes_until_nul(&line) {
            log_info!("console: queued \"%s\" at %.2f s", shown, script().elapsed());
        }
    }
}
