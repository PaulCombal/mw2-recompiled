// The image pool's block move (runtime/image_move.cpp's hook; the copy and its queue are
// src/runtime/gpu/image_move.rs, which says why the copy waits for its draw).

use crate::guest::{Register, guest_hook};
use crate::runtime::gpu::{gpu, image_move};
use crate::runtime::kernel;
use crate::title::*;

guest_hook!(T_Image_FlushMove, |ctx, memory| {
    let (destination, source, size) = (ctx.r3.as_u32(), ctx.r4.as_u32(), ctx.r5.as_u32());
    let physical = kernel::to_physical(destination);
    // Queued before the title's own call writes the copy draw, so the parse never meets
    // the draw before it knows what the draw copies.
    let at_its_draw =
        destination != 0 && source != 0 && size != 0 && physical != 0 && physical & 3 == 0 && gpu::consuming();
    if at_its_draw {
        image_move::queue(destination, source, size, 0x4000_0000 | (physical >> 2), gpu::frame_count());
    }
    T_Image_FlushMove.original(ctx, memory);
    if destination == 0 || source == 0 || size == 0 {
        return;
    }
    image_move::asked(size);
    if at_its_draw {
        return;
    }
    // Nothing is consuming the command stream to order the copy against.
    image_move::copy_now(destination, source, size);
});
