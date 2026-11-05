// CVE-2018-10718 and its client-side twin -- the netchan Huffman decompressor.
//
// A netchan message's payload is Huffman-compressed. MSG_ReadBitsCompress
// (sub_822897C0) expands it one byte per decoded symbol, and its loop stops only
// when it has consumed the *input* bits -- it never checks the *output* against
// the buffer it is writing to. Huffman can expand, so a crafted payload decodes
// to far more bytes than the caller's buffer holds:
//
//   - the server reads a client's message into a 2048-byte buffer on its own
//     stack (SV_ParseClientMessage, sub_822BCB18): an overrun smashes the return
//     address -- a remote, near-pre-auth RCE on the host;
//   - the client reads the host's message into a 64 KB hunk buffer
//     (CL_ParseServerMessage, sub_8218DF10): a malicious host overruns the heap
//     arena -- the same bug, host to client.
//
// The cause is one shared function with no output limit, so the fix is there.
// The decompressor cannot see how big its destination is, so each caller
// declares the size of the buffer it passes (the title's own 2048 / 64 KB) in a
// thread-local before calling the original; the decompressor hook decodes with
// that ceiling and, rather than writing past it, rejects the whole message
// (returns 0, as iw4x's bounds-checked decompress does). A legitimate message
// always fits the engine's own buffer, so nothing real is turned away.
//
// The decode of each symbol stays the title's own Huffman_DecodeSymbol, so the
// bitstream is read exactly as the title reads it -- no second implementation to
// drift, and nothing host-width-dependent.

use crate::guest::{Register, guest_hook, preserving};
use crate::log::log_warn;
use crate::title::*;
use std::cell::Cell;
use std::sync::Once;

thread_local! {
    // The destination buffer size the current caller passed, or 0 when the
    // decompressor is reached from somewhere this file does not know (then it
    // behaves exactly as the original). Set around the original call below, by
    // hand: a longjmp out of a parse skips the frame, and would skip a guard's
    // drop as it skipped the C++'s destructor, and the next decode takes the value.
    static CEILING: Cell<u32> = const { Cell::new(0) };
}

static LOGGED: Once = Once::new();

// sub_822897C0(from = r3, to = r4, size = r5) -> bytes written (r3).
guest_hook!(T_MSG_ReadBitsCompress, |ctx, memory| {
    let ceiling = CEILING.replace(0); // one-shot: applies only to this call
    if ceiling == 0 {
        T_MSG_ReadBitsCompress.original(ctx, memory);
        return;
    }

    let from = ctx.r3.as_u32();
    let to = ctx.r4.as_u32();
    let size = ctx.r5.as_i32();
    let total_bits = if size > 0 { (size as u32).wrapping_mul(8) } else { 0 };

    let written = preserving(ctx, |ctx| {
        // Two 4-byte slots for Huffman_DecodeSymbol's out-pointers (the decoded byte
        // and the bit cursor), in a small guest frame of our own. r1 is moved below
        // them so the callee's own stack use cannot land on them, exactly as the
        // original holds them inside its frame.
        let sp = ctx.r1.as_u32();
        let byte = sp.wrapping_sub(12);
        let bit_position = sp.wrapping_sub(16);
        ctx.r1.u64 = u64::from(sp.wrapping_sub(64));
        memory.store_u32(bit_position, 0);

        let tree = memory.u32_at(T_DATA_HuffmanTree.wrapping_add(8));

        let mut written = 0u32;
        if total_bits != 0 {
            loop {
                if written >= ceiling {
                    return None; // one more byte would leave the buffer
                }
                ctx.r3.u64 = u64::from(tree);
                ctx.r4.u64 = u64::from(byte);
                ctx.r5.u64 = u64::from(from);
                ctx.r6.u64 = u64::from(bit_position);
                T_Huffman_DecodeSymbol.call(ctx, memory);
                memory.store_u8(to.wrapping_add(written), memory.u32_at(byte) as u8);
                written += 1;
                if memory.u32_at(bit_position) >= total_bits {
                    break; // all input consumed: it fit
                }
            }
        }
        Some(written)
    });

    ctx.r3.u64 = u64::from(written.unwrap_or_else(|| {
        LOGGED.call_once(|| {
            log_warn!("online: rejected an over-long Huffman message (> %u bytes) -- CVE-2018-10718", ceiling)
        });
        0 // empty message; the parser handles a zero-length read
    }));
});

#[cfg(T_SV_ParseClientMessage)]
guest_hook!(T_SV_ParseClientMessage, |ctx, memory| {
    CEILING.set(2048); // the client message's 2048-byte stack buffer
    T_SV_ParseClientMessage.original(ctx, memory);
    CEILING.set(0);
});

#[cfg(T_CL_ParseServerMessage)]
guest_hook!(T_CL_ParseServerMessage, |ctx, memory| {
    CEILING.set(0x10000); // the server message's 64 KB hunk buffer
    T_CL_ParseServerMessage.original(ctx, memory);
    CEILING.set(0);
});
