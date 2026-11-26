// The window's triangles (draw.rs) drawn with SDL's renderer, as Dear ImGui's SDL
// renderer backend drew them: the font atlas a picture of white pixels with its coverage
// as alpha, sampled smoothly, and each run of triangles drawn inside its rectangle, made
// whole pixels, with its colours as SDL's floats. tools/draw_diff.py holds the two to the
// same pixels on SDL's software renderer.
//
// Dear ImGui is Copyright (c) 2014-2025 Omar Cornut, under the MIT licence, whose text is
// in launcher/NOTICE.md.

use crate::atlas::Atlas;
use crate::draw::{DrawList, Point, Texture, Vertex, int};
use sdl3_sys::everything as sdl;
use std::ffi::c_int;
use std::mem::{offset_of, size_of};
use std::ptr;

/// The atlas as a picture for the renderer, or None when it would not make one.
pub fn atlas_texture(renderer: *mut sdl::SDL_Renderer, atlas: &Atlas) -> Option<Texture> {
    let (width, height) = (c_int::try_from(atlas.width).ok()?, c_int::try_from(atlas.height).ok()?);
    let pixels = atlas.rgba();
    // SAFETY: the renderer is the caller's and alive; the pixels are width × height RGBA.
    unsafe {
        let texture =
            sdl::SDL_CreateTexture(renderer, sdl::SDL_PIXELFORMAT_RGBA32, sdl::SDL_TEXTUREACCESS_STATIC, width, height);
        if texture.is_null() {
            return None;
        }
        sdl::SDL_UpdateTexture(texture, ptr::null(), pixels.as_ptr().cast(), width * 4);
        sdl::SDL_SetTextureBlendMode(texture, sdl::SDL_BLENDMODE_BLEND);
        sdl::SDL_SetTextureScaleMode(texture, sdl::SDL_SCALEMODE_LINEAR);
        Some(texture_of(texture))
    }
}

/// A picture of SDL's, for the list to draw.
pub fn texture_of(texture: *mut sdl::SDL_Texture) -> Texture {
    Texture(texture.expose_provenance())
}

pub fn sdl_texture(texture: Texture) -> *mut sdl::SDL_Texture {
    ptr::with_exposed_provenance_mut(texture.0)
}

/// Draws the list in a window of `size` (in the window's own units) whose pixels are
/// `scale` times as many, over what is drawn already; the renderer's viewport and clip are
/// put back as they were.
pub fn present(
    renderer: *mut sdl::SDL_Renderer,
    list: &DrawList,
    size: Point,
    scale: Point,
    colours: &mut Vec<sdl::SDL_FColor>,
) {
    if list.indices.is_empty() {
        return;
    }
    // SAFETY: the renderer is the caller's and alive; every pointer handed to SDL is into
    // the list's buffers or this function's locals, which outlive the calls, and every
    // index is below the vertices' count (draw.rs makes them so).
    unsafe {
        let (mut rsx, mut rsy) = (1.0f32, 1.0f32);
        sdl::SDL_GetRenderScale(renderer, &mut rsx, &mut rsy);
        let render_scale = point_of(if rsx == 1.0 { scale.x } else { 1.0 }, if rsy == 1.0 { scale.y } else { 1.0 });
        let fb_width = int(size.x * render_scale.x);
        let fb_height = int(size.y * render_scale.y);
        if fb_width == 0 || fb_height == 0 {
            return;
        }
        let viewport_set = sdl::SDL_RenderViewportSet(renderer);
        let clip_enabled = sdl::SDL_RenderClipEnabled(renderer);
        let mut old_viewport = sdl::SDL_Rect::default();
        let mut old_clip = sdl::SDL_Rect::default();
        sdl::SDL_GetRenderViewport(renderer, &mut old_viewport);
        sdl::SDL_GetRenderClipRect(renderer, &mut old_clip);
        sdl::SDL_SetRenderViewport(renderer, ptr::null());
        sdl::SDL_SetRenderClipRect(renderer, ptr::null());

        colours.clear();
        colours.extend(list.vertices.iter().map(|v| {
            let [r, g, b, a] = v.col.to_le_bytes();
            sdl::SDL_FColor {
                r: f32::from(r) / 255.0,
                g: f32::from(g) / 255.0,
                b: f32::from(b) / 255.0,
                a: f32::from(a) / 255.0,
            }
        }));
        let base = list.vertices.as_ptr().cast::<u8>();
        let xy = base.add(offset_of!(Vertex, pos)).cast::<f32>();
        let uv = base.add(offset_of!(Vertex, uv)).cast::<f32>();
        let stride = size_of::<Vertex>() as c_int;
        for command in &list.commands {
            let mut min = point_of(command.clip[0] * render_scale.x, command.clip[1] * render_scale.y);
            let mut max = point_of(command.clip[2] * render_scale.x, command.clip[3] * render_scale.y);
            if min.x < 0.0 {
                min.x = 0.0;
            }
            if min.y < 0.0 {
                min.y = 0.0;
            }
            if max.x > fb_width as f32 {
                max.x = fb_width as f32;
            }
            if max.y > fb_height as f32 {
                max.y = fb_height as f32;
            }
            if max.x <= min.x || max.y <= min.y {
                continue;
            }
            let clip = sdl::SDL_Rect { x: int(min.x), y: int(min.y), w: int(max.x - min.x), h: int(max.y - min.y) };
            sdl::SDL_SetRenderClipRect(renderer, &clip);
            sdl::SDL_RenderGeometryRaw(
                renderer,
                sdl_texture(command.texture),
                xy,
                stride,
                colours.as_ptr(),
                size_of::<sdl::SDL_FColor>() as c_int,
                uv,
                stride,
                list.vertices.len() as c_int,
                list.indices.as_ptr().add(command.first).cast(),
                command.count as c_int,
                size_of::<u32>() as c_int,
            );
        }
        sdl::SDL_SetRenderViewport(renderer, if viewport_set { &old_viewport } else { ptr::null() });
        sdl::SDL_SetRenderClipRect(renderer, if clip_enabled { &old_clip } else { ptr::null() });
    }
}

fn point_of(x: f32, y: f32) -> Point {
    Point { x, y }
}
