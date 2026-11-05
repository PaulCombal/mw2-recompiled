// The sizes the title draws at: its frame, which it shows -- the menus and the HUD
// are drawn into it -- and its scene, the 3D view it draws first and copies into the
// frame. The console's are a 1280x720 frame and a 1024x600 scene; a frame of another
// shape gives the picture that shape, and the title follows it, its menus and HUD
// included (hooks/render_size.rs). MW2_RENDER=<w>x<h> is the frame, and the renderer
// draws it MW2_SCALE times wider and taller. runtime/render_size.h has the same rules
// for the launcher.

use crate::env;
use crate::log::{log_info, log_warn};
use std::sync::OnceLock;

#[repr(C)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

pub const CONSOLE_FRAME: Size = Size { width: 1280, height: 720 };
pub const CONSOLE_SCENE: Size = Size { width: 1024, height: 600 };

// The title draws into EDRAM: 2048 tiles of 80x16 samples. A surface is a whole
// number of tiles wide, and as many rows of samples tall as its pixels times its
// samples (a 2x surface has two rows a pixel).
fn tiles(size: Size, samples: u32) -> u32 {
    (size.width.wrapping_add(79) / 80).wrapping_mul(size.height.wrapping_mul(samples).wrapping_add(15) / 16)
}

// The frame is drawn at 1x, its colour and its depth in EDRAM together.
fn frame_fits(frame: Size) -> bool {
    tiles(frame, 1).wrapping_mul(2) <= 2048
}

// The scene is drawn at the console's 2x, colour and depth together, and takes no
// more tiles than the console's does: what the title keeps in the rest beside it is
// not known.
fn scene_fits(scene: Size) -> bool {
    tiles(scene, 2) <= tiles(CONSOLE_SCENE, 2)
}

// A frame the title can be given: as large as the console's at most in either
// direction, and no smaller than its standard-definition 640 wide, a whole number of
// 16 pixels wide and 4 tall, and fitting EDRAM.
pub fn frame_valid(frame: Size) -> bool {
    (640..=1280).contains(&frame.width)
        && (360..=800).contains(&frame.height)
        && frame.width.is_multiple_of(16)
        && frame.height.is_multiple_of(4)
        && frame_fits(frame)
}

/// The scene that goes with a frame: the console's with the console's frame; else the
/// frame itself when it fits, or the largest of its shape, 16 pixels narrower at a
/// time, that does.
pub fn scene(frame: Size) -> Size {
    if frame == CONSOLE_FRAME {
        return CONSOLE_SCENE;
    }
    let mut width = frame.width / 16 * 16;
    while width >= 16 {
        let scene = Size { width, height: width * frame.height / frame.width / 2 * 2 };
        if scene_fits(scene) {
            return scene;
        }
        width -= 16;
    }
    CONSOLE_SCENE
}

/// Whether a frame is wider than 4:3.
pub fn widescreen(frame: Size) -> bool {
    frame.width * 3 > frame.height * 4
}

// %u: white space, a sign, and digits, read as strtoul reads them, in 64 bits that
// stop at the most there is, of which the low 32 are kept.
fn number(text: &mut &[u8]) -> Option<u32> {
    while let [b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r', rest @ ..] = text {
        *text = rest;
    }
    let negative = text.first() == Some(&b'-');
    if let [b'+' | b'-', rest @ ..] = text {
        *text = rest;
    }
    let digits = text.iter().take_while(|byte| byte.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let value = text[..digits]
        .iter()
        .try_fold(0u64, |value, &digit| value.checked_mul(10)?.checked_add(u64::from(digit - b'0')));
    *text = &text[digits..];
    Some(match value {
        Some(value) if negative => value.wrapping_neg() as u32,
        Some(value) => value as u32,
        None => u32::MAX,
    })
}

/// "960x600", as sscanf("%ux%u%c") finds exactly two; None for anything else.
pub fn parse(text: &[u8]) -> Option<Size> {
    let mut text = text;
    let width = number(&mut text)?;
    let [b'x', rest @ ..] = text else { return None };
    text = rest;
    let height = number(&mut text)?;
    text.is_empty().then_some(Size { width, height })
}

fn asked_frame() -> Size {
    let Some(asked) = env::text(c"MW2_RENDER") else { return CONSOLE_FRAME };
    let Some(size) = parse(asked.to_bytes()).filter(|&size| frame_valid(size)) else {
        log_warn!(
            "render: MW2_RENDER=%s is not a size the title can draw at (640x360 to 1280x800, \
             a whole number of 16 pixels wide and 4 tall); it draws the console's 1280x720",
            asked
        );
        return CONSOLE_FRAME;
    };
    #[cfg(not(T_R_VideoSizes))]
    {
        let _ = size;
        log_warn!(
            "render: the %s cannot be given another size than the console's; MW2_RENDER=%s is left out",
            crate::title::TITLE_NAME,
            asked
        );
        CONSOLE_FRAME
    }
    #[cfg(T_R_VideoSizes)]
    {
        if size != CONSOLE_FRAME {
            let made = scene(size);
            log_info!(
                "render: the title draws a %ux%u frame and a %ux%u scene (MW2_RENDER)",
                size.width,
                size.height,
                made.width,
                made.height
            );
        }
        size
    }
}

/// The frame MW2_RENDER asks for, or the console's when it asks for none, for one
/// frame_valid refuses, or when this build of the title cannot be given one.
pub fn frame() -> Size {
    static FRAME: OnceLock<Size> = OnceLock::new();
    *FRAME.get_or_init(asked_frame)
}
