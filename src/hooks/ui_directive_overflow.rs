// iw4x issue: UI_ReplaceDirective ("[{name}]") stack overflow.
//
// A HUD/menu string can embed directives of the form "[{name}]" that the UI
// expands to a looked-up value. UI_ReplaceDirective (sub_822F0230 in the
// multiplayer, sub_822CDAA0 in the campaign) walks the source string and, while
// it is inside a directive name, accumulates each name character into a
// 256-byte buffer on its own stack frame:
//
//     loc_...033C:
//         stb  r10,0(r29)        ; namebuf[k] = char
//         addi r29,r29,1         ; k++   -- no bound against namebuf+256
//
// The loop's only guard tests the *output* cursor (r30 >= dest+size-1), and the
// output cursor does not advance while a name is being read, so a directive
// name longer than 256 bytes runs the cursor off the buffer: past it sit a
// second 256-byte scratch, then the saved registers and return address of a
// 688-byte frame -> a stack smash. The source is a server-supplied HUD string
// (scoreboard / hud strings), so a malicious host reaches this on a client; the
// campaign's co-op (Spec Ops) host reaches a campaign client the same way.
//
// iw4x bounds the name copy. The recompiler has no mid-function hook, so this
// replaces the whole function with a faithful transcription of its parse loop
// -- streaming the source exactly as the original does, reusing the title's own
// directive lookup (sub_822F0140), bounded string copy (I_strncpyz,
// sub_823159B0) and message printer (sub_8227F5E0) -- with the one missing
// bound added: the name buffer never takes more than 255 characters (leaving
// room for the terminator). A legitimate directive name is a short token, so
// nothing real is truncated; an over-long name simply fails to resolve, which
// is how the engine already treats a name it cannot find.

use crate::guest::{Memory, PPCContext, Register, guest_hook};
use crate::title::{
    T_Com_PrintMessage, T_I_strncpyz, T_STR_DirectiveEmpty, T_STR_DirectiveNoEnd, T_UI_LookupDirective,
};

const NAME_MAX: u32 = 255; // 256-byte buffer, less the terminator

// sub_8227F5E0(channel = 13, fmt, arg) -- the title's own warning printer,
// called for a malformed directive exactly as the original does. Loop state
// lives in this function's locals, so clobbering ctx across the call is
// fine; r1 already sits below our scratch (see the hook).
fn print_directive_warning(ctx: &mut PPCContext, memory: Memory, format: u32, argument: u32) {
    ctx.r3.u64 = 13;
    ctx.r4.u64 = u64::from(format);
    ctx.r5.u64 = u64::from(argument);
    T_Com_PrintMessage.call(ctx, memory);
}

// UI_ReplaceDirective(ctx = r3, src = r4, dest = r5, destSize = r6).
guest_hook!(T_UI_ReplaceDirective, |ctx, memory| {
    let ui_context = ctx.r3.as_u32();
    let source = ctx.r4.as_u32();
    let dest = ctx.r5.as_u32();
    let dest_size = ctx.r6.as_i32();

    // Two 256-byte buffers of our own -- the directive name and the lookup
    // result -- carved below the incoming stack pointer, with r1 moved beneath
    // them (leaving a parameter-save area for the printer's varargs) so the
    // reused guest routines' frames cannot land on them.
    let stack = ctx.r1.as_u32();
    let name = stack.wrapping_sub(0x100);
    let value = stack.wrapping_sub(0x200);
    ctx.r1.u64 = u64::from(stack.wrapping_sub(0x300));

    let end = dest.wrapping_add(dest_size as u32);
    if dest_size > 0 {
        memory.store_u8(end.wrapping_sub(1), 0); // dest[destSize-1] = 0, as the original does
    }

    let mut p = source; // source cursor
    let mut out = dest; // output cursor
    let mut name_length = 0; // characters in the directive-name buffer
    let mut in_name = false;

    let mut c = memory.u8_at(p);
    while c != 0 {
        if out >= end.wrapping_sub(1) {
            break; // output buffer full
        }

        if c == b'[' && memory.u8_at(p.wrapping_add(1)) == b'{' {
            // begin a directive name
            in_name = true;
            name_length = 0;
            p = p.wrapping_add(1); // skip '[' (the trailing step below skips '{')
        } else if c == b'}' && memory.u8_at(p.wrapping_add(1)) == b']' {
            // end a directive name
            p = p.wrapping_add(1); // skip '}' (the trailing step below skips ']')
            in_name = false;
            memory.store_u8(name.wrapping_add(name_length), 0);
            if name_length == 0 {
                print_directive_warning(ctx, memory, T_STR_DirectiveEmpty, p);
            } else {
                // LookupDirective(uiCtx, name, value, 256)
                ctx.r3.u64 = u64::from(ui_context);
                ctx.r4.u64 = u64::from(name);
                ctx.r5.u64 = u64::from(value);
                ctx.r6.u64 = 256;
                T_UI_LookupDirective.call(ctx, memory);

                // I_strncpyz(out, value, remaining) then advance past the value
                let remaining = (dest_size as u32).wrapping_sub(out.wrapping_sub(dest));
                ctx.r3.u64 = u64::from(out);
                ctx.r4.u64 = u64::from(value);
                ctx.r5.u64 = u64::from(remaining);
                T_I_strncpyz.call(ctx, memory);
                while memory.u8_at(out) != 0 {
                    out = out.wrapping_add(1);
                }
            }
        } else if in_name {
            // *** the fix: accept at most NAME_MAX name characters, then stop
            // writing (the name still ends at its "}]"); the original stored
            // every character with no bound.
            if name_length < NAME_MAX {
                memory.store_u8(name.wrapping_add(name_length), c);
                name_length += 1;
            }
        } else {
            memory.store_u8(out, c);
            out = out.wrapping_add(1);
        }

        p = p.wrapping_add(1);
        c = memory.u8_at(p);
    }

    if out < end {
        memory.store_u8(out, 0); // terminate the output
    }
    if in_name {
        print_directive_warning(ctx, memory, T_STR_DirectiveNoEnd, p); // the string ended mid-directive
    }

    ctx.r1.u64 = u64::from(stack); // balance the stack the function was entered with
});
