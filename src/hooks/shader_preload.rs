// Every shader the title loads, handed to the renderer to compile before it is drawn with
// (runtime/shader_preload.cpp's hooks; the compiling is renderer_libraries.cpp's).
//
// MW2 streams its shaders out of the fastfiles with everything else a level needs: the
// loader thread inflates each material's programs into physical memory and makes a D3D
// shader object over each one. The console's GPU runs that microcode as it stands, so the
// shader is ready from then on; here it has to be translated and compiled, and done at the
// first draw instead it was a stutter wherever something new came on screen.
//
// D3D's shader object is made in place by one routine per stage, given the object and the
// program -- the physical block the loader read the shader into. The object holds the
// shader's header (magic 0x102A11xx): at +40 in a pixel shader's, at +872 in a vertex
// shader's, where the vertex fetch tables come first. The header's seventh word is the
// offset, from the header, of a table whose first two words are where the microcode starts
// in the program (0, 64 or 128 bytes in) and how many bytes it is. That is exactly what
// D3D later points the command stream's IM_LOAD at.
//
// A vertex shader is not drawn as it is made. Its fetches name no format: the title binds
// each one to a vertex declaration while it loads the material pass
// (IDirect3DVertexShader9::Bind), and D3D rewrites the fetches in place -- fetch constant,
// format, stride and offset. Two more D3D paths do the same (the image pool's memory move
// is bound by one, and one patches a copy it writes into a command buffer), all through
// one routine. Every vertex shader the stream loads is a patched one, so a vertex shader
// is handed over once that routine has patched it, and not as it is made: the shader as
// made is never drawn, and compiling it was work for nothing.

use crate::guest::{Memory, Register, guest_hook};
use crate::log::log_warn;
use crate::runtime::bridge::renderer;
use crate::title::*;
use std::sync::atomic::{AtomicU32, Ordering};

static UNREADABLE: AtomicU32 = AtomicU32::new(0);

// `microcode` is where the code is, when not at its place in `program`: a patched copy D3D
// wrote into a command buffer.
fn hand(memory: Memory, pixel: bool, object: u32, program: u32, microcode: u32) {
    let header = object.wrapping_add(if pixel { 40 } else { 872 });
    let magic = memory.u32_at(header);
    let table = header.wrapping_add(memory.u32_at(header.wrapping_add(24)));
    let (offset, bytes) = (memory.u32_at(table), memory.u32_at(table.wrapping_add(4)));
    if magic & 0xFFFF_FF00 != 0x102A_1100
        || (program == 0 && microcode == 0)
        || bytes == 0
        || bytes & 3 != 0
        || bytes > 256 << 10
        || offset > 4096
    {
        if UNREADABLE.fetch_add(1, Ordering::SeqCst) < 4 {
            log_warn!(
                "shaders: a %s shader object at %08X has no header this can read (%08X, microcode %u bytes at +%u); it is compiled at its first draw",
                if pixel { c"pixel" } else { c"vertex" },
                object,
                magic,
                bytes,
                offset
            );
        }
        return;
    }
    // The command stream hands the renderer the microcode as native dwords.
    let bound = microcode != 0;
    let microcode = if bound { microcode } else { program.wrapping_add(offset) };
    let code: Vec<u32> = (0..bytes / 4).map(|i| memory.u32_at(microcode.wrapping_add(i * 4))).collect();
    renderer::prepare_shader(pixel, &code);
    #[cfg(feature = "diagnostics")]
    {
        use crate::log::log_info;
        use std::sync::OnceLock;
        static TRACED: OnceLock<bool> = OnceLock::new();
        if *TRACED.get_or_init(|| crate::env::flag(c"MW2_TRACE_SHADER_LOADS")) {
            log_info!(
                "shaders: %s shader %s at %08X, %016llX",
                if pixel { c"pixel" } else { c"vertex" },
                if bound { c"bound" } else { c"made" },
                microcode,
                crate::runtime::gpu::hash::shader_hash(&code)
            );
        }
    }
    #[cfg(not(feature = "diagnostics"))]
    let _ = bound;
}

guest_hook!(T_D3D_InitPixelShader, |ctx, memory| {
    let (object, program) = (ctx.r3.as_u32(), ctx.r4.as_u32());
    T_D3D_InitPixelShader.original(ctx, memory);
    hand(memory, true, object, program, 0);
});

// (shader, where its microcode is, declaration, stream strides, ...): the microcode is the
// shader's own, or a copy in a command buffer.
#[cfg(T_D3D_PatchVertexFetches)]
guest_hook!(T_D3D_PatchVertexFetches, |ctx, memory| {
    let (object, microcode) = (ctx.r3.as_u32(), ctx.r4.as_u32());
    T_D3D_PatchVertexFetches.original(ctx, memory);
    hand(memory, false, object, 0, microcode);
});
