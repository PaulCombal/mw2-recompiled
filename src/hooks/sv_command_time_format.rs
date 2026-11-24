// A client's name is used as a print format when its command time is refused.
//
// The server checks the time each client command carries (sub_822BC778). When
// it is more than 20 seconds ahead of the server's own, the function says so
// and drops the command:
//
//     va("Invalid command time %i from client %s, current server time is %i\n",
//        commandTime, client->name, serverTime)
//     Com_PrintMessage(15, <that text>)
//
// The finished text, the client's name in it, is handed to the printer as its
// format. The name is the client's own choice and so is the command time, so a
// client in the match can have whatever '%' directives it puts in its name
// interpreted on the host, with no arguments behind them.
//
// The message is printed here instead, the same text through "%s", and the
// command is dropped as before. Any other command time goes to the original
// function untouched. The text is still built by the title's own va, from the
// title's own format.
//
// Multiplayer only: the campaign has no such check.

use crate::guest::{Register, guest_hook, preserving};
use crate::title::{
    T_Com_PrintMessage, T_DATA_ServerTime, T_STR_InvalidCommandTime, T_STR_PercentS, T_SV_CheckCommandTime, T_va,
};

const CLIENT_NAME: u32 = 0x212B8; // client->name
const FAR_AHEAD: i32 = 20000; // milliseconds, the function's own limit

// sub_822BC778(client = r3, command = r4); the command's time is its first word.
guest_hook!(T_SV_CheckCommandTime, |ctx, memory| {
    let client = ctx.r3.as_u32();
    let command_time = memory.u32_at(ctx.r4.as_u32());
    let server_time = memory.u32_at(T_DATA_ServerTime);
    // The function's own test: a signed difference of the two 32-bit times.
    if command_time.wrapping_sub(server_time) as i32 <= FAR_AHEAD {
        T_SV_CheckCommandTime.original(ctx, memory);
        return;
    }

    preserving(ctx, |ctx| {
        // A frame of our own for the two calls, as the function has one: both are
        // variadic and spill their register arguments into their caller's frame.
        ctx.r1.u64 = u64::from(ctx.r1.as_u32().wrapping_sub(0x80));

        ctx.r3.u64 = u64::from(T_STR_InvalidCommandTime);
        ctx.r4.u64 = u64::from(command_time);
        ctx.r5.u64 = u64::from(client.wrapping_add(CLIENT_NAME));
        ctx.r6.u64 = u64::from(server_time);
        T_va.call(ctx, memory);

        ctx.r5.u64 = u64::from(ctx.r3.as_u32()); // the text, now an argument
        ctx.r4.u64 = u64::from(T_STR_PercentS); // "%s"
        ctx.r3.u64 = 15;
        T_Com_PrintMessage.call(ctx, memory);
    });
});
