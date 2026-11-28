# Notice

Four files of the launcher are ports of other projects' code and keep their licences. (The map
converter, `../mapport/`, and the zone definitions, `../zone/defs/` and `../zone/defs_pc/`, have
notices of their own.)

## src/draw.rs, src/atlas.rs, src/present.rs, src/input.rs: Dear ImGui, MIT

src/draw.rs is a Rust port of what the launcher's window drew with of Dear ImGui 1.91.9b's draw list
(`imgui_draw.cpp`: `ImDrawList`'s lines, rectangles, quadrilaterals, circles, gradients, images,
polylines and convex polygons, and `ImFont`'s text layout and rendering, with `imgui.cpp`'s UTF-8
decoder); src/atlas.rs of its font atlas built with stb_truetype (`ImFontAtlasBuildWithStbTruetype`
and what it calls); src/present.rs of its SDL renderer backend's drawing
(`backends/imgui_impl_sdlrenderer3.cpp`); and src/input.rs of what the window read of the keyboard,
a controller and the pointer through its IO (`imgui.cpp`'s input queue, key repeat and mouse state)
and its SDL3 backend (`backends/imgui_impl_sdl3.cpp`'s events, mouse, keys and gamepads). Each does
what that code does, in the same order, so that the window is drawn to the same pixels and answers
the same keys. Its notice:

> The MIT License (MIT)
>
> Copyright (c) 2014-2025 Omar Cornut
>
> Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
> associated documentation files (the "Software"), to deal in the Software without restriction,
> including without limitation the rights to use, copy, modify, merge, publish, distribute,
> sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is
> furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all copies or
> substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT
> NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
> NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
> DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT
> OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

## src/atlas.rs: stb_rect_pack, public domain

src/atlas.rs also ports stb_rect_pack 1.01's skyline packer, in the copy that comes with Dear ImGui
(`imstb_rectpack.h`), Sean Barrett's, which is in the public domain or, at the user's choice, under
the MIT licence; it is taken here under the former. The rasteriser the atlas is drawn with is
stb_truetype's, ported in the `mw2-truetype` crate (`truetype/`), taken the same way.
