// MW2_RENDER=<w>x<h>: the frame the title draws, in place of the console's 1280x720,
// and so the picture's shape (runtime/render_size.rs).
//
// The title takes its sizes from no video mode: R_VideoSizes writes the console's
// own, a 1024x600 scene and a 1280x720 frame, whenever the mode is high definition,
// and R_SetVidConfig gives the picture an aspect of 16/9 or 4/3 by the mode's
// widescreen flag alone. Everything after reads the vidConfig it makes: D3D's back
// buffer and render targets are made at its sizes, and the views' projections, the
// HUD's and the menus' placement and the cinematics' letterbox follow its aspect --
// the PC game's engine, which draws at any shape. So the sizes are written after
// R_VideoSizes, and the aspect after R_SetVidConfig: the frame's width over its
// height, square pixels, and the scene's pixel aspect from it. A cinematic is
// letterboxed by the title itself, whole between black bars.
//
// A 2D picture made for the console's 16:9 screen and drawn over the whole frame -- a
// loading screen, a menu's background, a full-screen overlay -- covers the frame
// instead of stretching to it: it is as large as the frame where the shapes differ,
// and what spills over is cut, as a 16:9 picture on a 16:10 screen is cut at its
// sides. It is one whose every texture was loaded from a file: a copy of the screen
// (a render target), which the menus blur behind them, is drawn over the frame too,
// and must be left whole.
//
// The menus lay some things out over the screen's width in their own units, which
// they take as 640 + 213.333 * dvarbool(widescreen): 640 for 4:3 and 853.33 for 16:9,
// the console's two shapes -- the experience bar's width, for one, which ran off a
// 16:10 screen. A constant a menu expression multiplies dvarbool(widescreen) by is
// scaled by the share of the way from 4:3 to 16:9 the picture's shape is
// (UI_EvaluateStatement): 0.6 at 16:10, which makes that width 768, the 16:10
// screen's. dvarbool(widescreen) itself stays 1: the menus also test it, the loading
// screen's bar for one, and a test truncates what it is given to a whole number.
//
// With the console's frame none of this does anything.

use crate::guest::{Memory, PPCContext, PPCRegister, Register, guest_hook};
use crate::log::log_info;
use crate::runtime::render_size::{self, CONSOLE_FRAME};
use crate::title::*;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};

const CONSOLE_ASPECT: f32 = 16.0 / 9.0;

// The vidConfig's fields
const SCENE_WIDTH: u32 = 0;
const SCENE_HEIGHT: u32 = 4;
const FRAME_WIDTH: u32 = 8;
const FRAME_HEIGHT: u32 = 12;
const ASPECT: u32 = 28;
const SCENE_PIXEL_ASPECT: u32 = 32;
const FRAME_PIXEL_ASPECT: u32 = 36;
// R_VideoSizes's
const HI_DEF: u32 = 1;
const SCENE: u32 = 8;
const FRAME: u32 = 16;
// A Material's and a GfxImage's, on the 360 (launcher/zone/defs/iw4_assets.h)
const MATERIAL_TEXTURE_COUNT: u32 = 57;
const MATERIAL_TEXTURE_TABLE: u32 = 68;
const TEXTURE_DEF_SIZE: u32 = 12;
const TEXTURE_DEF_IMAGE: u32 = 8;
const IMAGE_CATEGORY: u32 = 58;
const LOADED_FROM_FILE: u8 = 3; // IMG_CATEGORY_LOAD_FROM_FILE
// R_DrawStretchPic's material, past its eight floats, in the caller's parameter area
const STRETCH_PIC_MATERIAL: u32 = 92;
// An expression's Statement_s, its ExpressionSupportingData's static dvar list, a
// StaticDvar { dvar_t*, name }, and an Operand { dataType, value }
const STATEMENT_SUPPORTING_DATA: u32 = 8;
const SUPPORTING_STATIC_DVARS: u32 = 12;
const STATIC_DVAR_NAME: u32 = 4;
const OPERAND_INT: u32 = 0;
const OPERAND_FLOAT: u32 = 1;
// The statement's entries, { type, op or Operand }, 12 bytes each: an operator (type
// 0) or an operand (type 1, an Operand { dataType, value }).
const STATEMENT_ENTRY_COUNT: u32 = 0;
const STATEMENT_ENTRIES: u32 = 4;
const ENTRY_SIZE: u32 = 12;
const OPERATOR: u32 = 0;
const OP_RIGHT_PAREN: u32 = 1;
const OP_MULTIPLY: u32 = 2;
const OP_STATIC_DVAR_BOOL: u32 = 24;

// Whether every texture of `material` was loaded from a file: a picture, not a copy
// of the screen.
fn loaded(memory: Memory, material: u32) -> bool {
    let count = u32::from(memory.u8_at(material.wrapping_add(MATERIAL_TEXTURE_COUNT)));
    let table = memory.u32_at(material.wrapping_add(MATERIAL_TEXTURE_TABLE));
    if count == 0 || table == 0 {
        return false;
    }
    (0..count).all(|i| {
        let image = memory.u32_at(table.wrapping_add(i * TEXTURE_DEF_SIZE).wrapping_add(TEXTURE_DEF_IMAGE));
        image != 0 && memory.u8_at(image.wrapping_add(IMAGE_CATEGORY)) == LOADED_FROM_FILE
    })
}

// Says once a material which picture was made to cover the frame.
fn say(memory: Memory, material: u32) {
    static SAID: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());
    let mut said = SAID.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if said.len() >= 64 || !said.insert(material) {
        return;
    }
    match memory.pointer_at(material) {
        Some(name) => log_info!("render: a picture covers the frame (%.64s)", memory.text(name)),
        None => log_info!("render: a picture covers the frame (unnamed)"),
    }
}

fn cut(from: &mut PPCRegister, to: &mut PPCRegister, kept: f32) {
    let (start, end) = (from.as_f64(), to.as_f64());
    let middle = (start + end) * 0.5;
    from.f64 = f64::from((middle + (start - middle) * f64::from(kept)) as f32);
    to.f64 = f64::from((middle + (end - middle) * f64::from(kept)) as f32);
}

// Cuts a 16:9 picture's coordinates to the part of it a frame of `aspect` shows when
// the picture covers it.
fn cover(ctx: &mut PPCContext, aspect: f32) {
    if aspect < CONSOLE_ASPECT {
        cut(&mut ctx.f5, &mut ctx.f7, aspect / CONSOLE_ASPECT);
    } else if aspect > CONSOLE_ASPECT {
        cut(&mut ctx.f6, &mut ctx.f8, CONSOLE_ASPECT / aspect);
    }
}

fn entry(entries: u32, i: u32) -> u32 {
    entries.wrapping_add(i.wrapping_mul(ENTRY_SIZE))
}

// Whether entry `at` is dvarbool(static) of `widescreen`: the operator, its index
// into the statement's static dvar list, and the closing paren.
fn is_widescreen_test(memory: Memory, statement: u32, entries: u32, count: u32, at: u32) -> bool {
    let word = |address: u32| memory.u32_at(address);
    if at.wrapping_add(2) >= count
        || word(entry(entries, at)) != OPERATOR
        || word(entry(entries, at).wrapping_add(4)) != OP_STATIC_DVAR_BOOL
        || word(entry(entries, at.wrapping_add(1))) == OPERATOR
        || word(entry(entries, at.wrapping_add(1)).wrapping_add(4)) != OPERAND_INT
    {
        return false;
    }
    let list = word(word(statement.wrapping_add(STATEMENT_SUPPORTING_DATA)).wrapping_add(SUPPORTING_STATIC_DVARS));
    let index = word(entry(entries, at.wrapping_add(1)).wrapping_add(8));
    let dvar = word(list.wrapping_add(index.wrapping_mul(4)));
    let name = if dvar != 0 { word(dvar.wrapping_add(STATIC_DVAR_NAME)) } else { 0 };
    name != 0 && memory.holds_any_case(name, c"widescreen")
}

// Scales the constants the statement multiplies dvarbool(widescreen) by:
// `c * dvarbool(widescreen)` and `dvarbool(widescreen) * c`.
fn scale_widescreen_terms(memory: Memory, statement: u32, share: f32) {
    static MADE: OnceLock<Mutex<HashMap<u32, u32>>> = OnceLock::new();
    let made = MADE.get_or_init(Default::default);
    let count = memory.u32_at(statement.wrapping_add(STATEMENT_ENTRY_COUNT));
    let entries = memory.u32_at(statement.wrapping_add(STATEMENT_ENTRIES));
    let is_op = |i: u32, op: u32| {
        i < count
            && memory.u32_at(entry(entries, i)) == OPERATOR
            && memory.u32_at(entry(entries, i).wrapping_add(4)) == op
    };
    let scale = |i: u32| {
        let at = entry(entries, i);
        if memory.u32_at(at) == OPERATOR {
            return;
        }
        let kind = memory.u32_at(at.wrapping_add(4));
        let bits = memory.u32_at(at.wrapping_add(8));
        if kind != OPERAND_INT && kind != OPERAND_FLOAT {
            return;
        }
        let mut made = made.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if made.get(&at) == Some(&bits) && kind == OPERAND_FLOAT {
            return;
        }
        let value = if kind == OPERAND_FLOAT { f32::from_bits(bits) } else { bits as i32 as f32 };
        let scaled = (value * share).to_bits();
        memory.store_u32(at.wrapping_add(4), OPERAND_FLOAT);
        memory.store_u32(at.wrapping_add(8), scaled);
        made.insert(at, scaled);
    };
    let mut i = 0;
    while i < count {
        if is_widescreen_test(memory, statement, entries, count, i) {
            if i >= 2 && is_op(i - 1, OP_MULTIPLY) {
                scale(i - 2);
            }
            if is_op(i.wrapping_add(2), OP_RIGHT_PAREN)
                && is_op(i.wrapping_add(3), OP_MULTIPLY)
                && i.wrapping_add(4) < count
            {
                scale(i.wrapping_add(4));
            }
        }
        i += 1;
    }
}

// How far the picture's shape is from 4:3 towards 16:9: 1 at 16:9. A frame no wider
// than 4:3 leaves the constants be: dvarbool(widescreen) is 0 then, the video mode's.
fn widescreen_share() -> f32 {
    static SHARE: OnceLock<f32> = OnceLock::new();
    *SHARE.get_or_init(|| {
        let frame = render_size::frame();
        if !render_size::widescreen(frame) {
            return 1.0;
        }
        let aspect = frame.width as f32 / frame.height as f32;
        (aspect - 4.0 / 3.0) / (CONSOLE_ASPECT - 4.0 / 3.0)
    })
}

guest_hook!(T_R_VideoSizes, |ctx, memory| {
    let config = ctx.r3.as_u32();
    T_R_VideoSizes.original(ctx, memory);
    let frame = render_size::frame();
    if frame == CONSOLE_FRAME || memory.u8_at(config.wrapping_add(HI_DEF)) == 0 {
        return;
    }
    let scene = render_size::scene(frame);
    memory.store_u32(config.wrapping_add(SCENE), scene.width);
    memory.store_u32(config.wrapping_add(SCENE + 4), scene.height);
    memory.store_u32(config.wrapping_add(FRAME), frame.width);
    memory.store_u32(config.wrapping_add(FRAME + 4), frame.height);
});

guest_hook!(T_R_SetVidConfig, |ctx, memory| {
    T_R_SetVidConfig.original(ctx, memory);
    if render_size::frame() == CONSOLE_FRAME {
        return;
    }
    // As the title works them out, in single precision.
    let config = T_DATA_VidConfig;
    let scene_width = memory.u32_at(config + SCENE_WIDTH) as f32;
    let scene_height = memory.u32_at(config + SCENE_HEIGHT) as f32;
    let frame_width = memory.u32_at(config + FRAME_WIDTH) as f32;
    let frame_height = memory.u32_at(config + FRAME_HEIGHT) as f32;
    let aspect = frame_width / frame_height;
    memory.store_u32(config + ASPECT, aspect.to_bits());
    memory.store_u32(config + SCENE_PIXEL_ASPECT, (scene_height * aspect / scene_width).to_bits());
    memory.store_u32(config + FRAME_PIXEL_ASPECT, (frame_height * aspect / frame_width).to_bits());
});

guest_hook!(T_R_DrawStretchPic, |ctx, memory| {
    if render_size::frame() != CONSOLE_FRAME {
        let config = T_DATA_VidConfig;
        let width = f64::from(memory.u32_at(config + FRAME_WIDTH));
        let height = f64::from(memory.u32_at(config + FRAME_HEIGHT));
        let aspect = f32::from_bits(memory.u32_at(config + ASPECT));
        let material = memory.u32_at(ctx.r1.as_u32().wrapping_add(STRETCH_PIC_MATERIAL));
        let (x, y, w, h) = (ctx.f1.as_f64(), ctx.f2.as_f64(), ctx.f3.as_f64(), ctx.f4.as_f64());
        if material != 0
            && x <= 0.5
            && y <= 0.5
            && x + w >= width - 0.5
            && y + h >= height - 0.5
            && loaded(memory, material)
        {
            cover(ctx, aspect);
            say(memory, material);
        }
    }
    T_R_DrawStretchPic.original(ctx, memory);
});

// UI_EvaluateStatement(localClientNum, statement): a menu expression's value. Before
// it runs, a constant it multiplies dvarbool(widescreen) by is scaled by the shape's
// share. Each constant is scaled once: one that holds what it was made is left, and
// one a zone loaded again over it is made again.
guest_hook!(T_UI_EvaluateStatement, |ctx, memory| {
    let share = widescreen_share();
    if share != 1.0 {
        scale_widescreen_terms(memory, ctx.r4.as_u32(), share);
    }
    T_UI_EvaluateStatement.original(ctx, memory);
});
