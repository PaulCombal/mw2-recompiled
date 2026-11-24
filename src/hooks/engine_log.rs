// Surfaces IW4's own diagnostics. The engine funnels them through Com_Error and
// Com_Printf, both printf-style, so hooking them turns the title's internal reasons for
// giving up into runtime output. Com_Error is always reported; the chattier print goes
// behind MW2_ENGINE_LOG.
//
// The line is made and gone before the original runs: Com_Error does not come back.

use crate::env;
use crate::guest::{Memory, PPCContext, Register, guest_hook};
use crate::log::{c_string, format_text, log_error};
use crate::runtime::platform;
use std::ffi::CString;
use std::sync::LazyLock;

const CONVERSIONS: &[u8] = b"diuxXcsfgeEp";

fn guest_bytes(memory: Memory, address: u32, out: &mut Vec<u8>) {
    let mut at = address;
    loop {
        let byte = memory.u8_at(at);
        if byte == 0 {
            return;
        }
        out.push(byte);
        at = at.wrapping_add(1);
    }
}

// PowerPC passes the first eight integer arguments in r3..r10, so a format with up to six
// varargs needs no guest stack walking.
fn format(ctx: &PPCContext, memory: Memory, address: u32) -> CString {
    if address == 0 {
        return c"(null format)".to_owned();
    }
    let arguments = [ctx.r5, ctx.r6, ctx.r7, ctx.r8, ctx.r9, ctx.r10].map(|register| register.as_u64());
    let mut used = 0;
    let mut out = Vec::new();
    let mut at = address;
    loop {
        let byte = memory.u8_at(at);
        if byte == 0 {
            break;
        }
        at = at.wrapping_add(1);
        if byte != b'%' {
            out.push(byte);
            continue;
        }
        if memory.u8_at(at) == b'%' {
            out.push(b'%');
            at = at.wrapping_add(1);
            continue;
        }
        // The engine's messages do not use flags, width or precision.
        let conversion = loop {
            let byte = memory.u8_at(at);
            if byte == 0 || CONVERSIONS.contains(&byte) {
                break byte;
            }
            at = at.wrapping_add(1);
        };
        if conversion == 0 {
            break;
        }
        at = at.wrapping_add(1);
        let Some(&raw) = arguments.get(used) else {
            out.extend_from_slice(b"<arg spilled>");
            continue;
        };
        used += 1;
        let value = raw as u32;
        match conversion {
            b's' if value != 0 => guest_bytes(memory, value, &mut out),
            b's' => out.extend_from_slice(b"(null)"),
            b'c' => out.push(value as u8),
            b'd' | b'i' => out.extend_from_slice(format_text!("%d", value as i32).as_bytes()),
            b'u' => out.extend_from_slice(format_text!("%u", value).as_bytes()),
            b'x' => out.extend_from_slice(format_text!("%x", value).as_bytes()),
            b'X' | b'p' => out.extend_from_slice(format_text!("%X", value).as_bytes()),
            // A variadic double travels in the next GPR, whole: viewpos stores each one
            // and loads it back with ld before the call.
            _ => out.extend_from_slice(format_text!("%g", f64::from_bits(raw)).as_bytes()),
        }
    }
    while matches!(out.last(), Some(b'\n' | b'\r')) {
        out.pop();
    }
    c_string(out)
}

static TRACE_ERRORS: LazyLock<bool> = LazyLock::new(|| cfg!(feature = "diagnostics") && env::flag(c"MW2_TRACE_ERRORS"));

guest_hook!(T_Com_Error, |ctx, memory| {
    // The caller's address: the same message is raised from several places, and which one
    // gave up is half the answer.
    let line = format(ctx, memory, ctx.r4.as_u32());
    log_error!("Com_Error(%u) from %08X: %s", ctx.r3.as_u32(), ctx.lr as u32, line.as_c_str());
    drop(line);
    if *TRACE_ERRORS {
        platform::print_backtrace(c"the call stack that gave up");
    }
    crate::title::T_Com_Error.original(ctx, memory);
});

#[cfg(feature = "diagnostics")]
mod traces {
    use super::*;
    use crate::log::{log_info, log_kernel};

    static VERBOSE: LazyLock<bool> = LazyLock::new(|| env::flag(c"MW2_ENGINE_LOG"));
    static TRACE_CBUF: LazyLock<bool> = LazyLock::new(|| env::flag(c"MW2_TRACE_CBUF"));

    // Com_Printf(channel, format, ...): viewpos answers through it, so it is the general
    // print and not only the error one.
    guest_hook!(T_Com_Printf, |ctx, memory| {
        if *VERBOSE {
            log_kernel!("Com_Printf(%u): %s", ctx.r3.as_u32(), format(ctx, memory, ctx.r4.as_u32()).as_c_str());
        }
        crate::title::T_Com_Printf.original(ctx, memory);
    });

    // The title's other prints, with the same arguments: its parsers report through these,
    // and without them a playlist that fails says nothing.
    #[cfg(T_Com_PrintError)]
    guest_hook!(T_Com_DPrintf, |ctx, memory| {
        if *VERBOSE {
            log_kernel!("Com_DPrintf(%u): %s", ctx.r3.as_u32(), format(ctx, memory, ctx.r4.as_u32()).as_c_str());
        }
        crate::title::T_Com_DPrintf.original(ctx, memory);
    });

    #[cfg(T_Com_PrintError)]
    guest_hook!(T_Com_PrintError, |ctx, memory| {
        if *VERBOSE {
            log_kernel!("Com_PrintError(%u): %s", ctx.r3.as_u32(), format(ctx, memory, ctx.r4.as_u32()).as_c_str());
        }
        crate::title::T_Com_PrintError.original(ctx, memory);
    });

    #[cfg(T_Com_PrintError)]
    guest_hook!(T_Com_PrintWarning, |ctx, memory| {
        if *VERBOSE {
            log_kernel!("Com_PrintWarning(%u): %s", ctx.r3.as_u32(), format(ctx, memory, ctx.r4.as_u32()).as_c_str());
        }
        crate::title::T_Com_PrintWarning.original(ctx, memory);
    });

    // Cbuf_AddText(localClientNum, text): what the menus and the title's own code queue
    // for the console. MW2_TRACE_CBUF shows the commands a menu runs, so a flow the player
    // clicks through can be typed instead.
    guest_hook!(T_Cbuf_AddText, |ctx, memory| {
        if *TRACE_CBUF {
            let mut text = Vec::new();
            if ctx.r4.as_u32() != 0 {
                guest_bytes(memory, ctx.r4.as_u32(), &mut text);
            }
            log_info!("cbuf(%u): %s", ctx.r3.as_u32(), c_string(text).as_c_str());
        }
        crate::title::T_Cbuf_AddText.original(ctx, memory);
    });
}

// DB_FindXAssetHeader gives up here when a lookup fails and the loader is not running:
// DB_MissingAsset(type, name).
#[cfg(T_DB_MissingAsset)]
guest_hook!(T_DB_MissingAsset, |ctx, memory| {
    let (kind, address) = (ctx.r3.as_u32(), ctx.r4.as_u32());
    let name = if address != 0 { Some(memory.text(address)) } else { None };
    let thread = crate::runtime::crash::current_thread_name().unwrap_or(c"?");
    match name {
        Some(name) => log_error!("db: missing asset type %u '%s' on %s", kind, name, thread),
        None => log_error!("db: missing asset type %u '%s' on %s", kind, c"(null)", thread),
    }
    crate::title::T_DB_MissingAsset.original(ctx, memory);
});
