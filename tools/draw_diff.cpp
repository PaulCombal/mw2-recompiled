// tools/draw_diff.py's C++ side: Dear ImGui 1.91.9b, as the launcher's window drew with it
// before (launcher/ui.cpp at 8f5c04e): its fonts loaded into the atlas as ui::LoadFonts
// loaded them, a frame begun as main.cpp began one, and the background draw list drawn on.
// draw_diff.rs calls the same functions of its Rust (launcher/src/atlas.rs, draw.rs) and
// compares what each made.
#include "imgui.h"
#include "imgui_impl_sdlrenderer3.h"
#include "imgui_internal.h"

#include <SDL3/SDL.h>

#include <cfloat>
#include <cstdint>
#include <cstring>
#include <vector>

namespace
{
    ImFont* fonts[16];
    ImDrawList* list = nullptr;
    std::vector<unsigned char> flat;

    ImVec2 P(const float* p) { return { p[0], p[1] }; }
}

extern "C"
{
    // The atlas, as ui::LoadFonts filled it: each font's data (the executable's, so not
    // owned by the atlas) at a size in pixels, sampled `oversample` times across.
    int cpp_init(int count, const unsigned char* const* data, const int* sizes, const float* pixels, int oversample)
    {
        ImGui::CreateContext();
        ImGuiIO& io = ImGui::GetIO();
        io.IniFilename = nullptr;
        ImFontConfig config;
        config.FontDataOwnedByAtlas = false;
        config.OversampleH = oversample;
        for (int i = 0; i < count; i++)
            fonts[i] = io.Fonts->AddFontFromMemoryTTF(const_cast<unsigned char*>(data[i]), sizes[i], pixels[i], &config);
        unsigned char* rgba;
        int width, height;
        io.Fonts->GetTexDataAsRGBA32(&rgba, &width, &height);
        io.Fonts->SetTexID((ImTextureID)1);
        return io.Fonts->TexPixelsAlpha8 != nullptr;
    }

    void cpp_shutdown() { ImGui::DestroyContext(); }

    // The atlas's coverage, its size, the white pixel, the lines' bands and the room
    // kept for the mouse cursors (x, y, w, h).
    const unsigned char* cpp_atlas(int* width, int* height, float* white, float* lines, int* cursors)
    {
        ImFontAtlas* atlas = ImGui::GetIO().Fonts;
        *width = atlas->TexWidth;
        *height = atlas->TexHeight;
        white[0] = atlas->TexUvWhitePixel.x;
        white[1] = atlas->TexUvWhitePixel.y;
        for (int n = 0; n <= IM_DRAWLIST_TEX_LINES_WIDTH_MAX; n++)
        {
            lines[n * 4 + 0] = atlas->TexUvLines[n].x;
            lines[n * 4 + 1] = atlas->TexUvLines[n].y;
            lines[n * 4 + 2] = atlas->TexUvLines[n].z;
            lines[n * 4 + 3] = atlas->TexUvLines[n].w;
        }
        const ImFontAtlasCustomRect* r = atlas->GetCustomRectByIndex(atlas->PackIdMouseCursors);
        cursors[0] = r->X;
        cursors[1] = r->Y;
        cursors[2] = r->Width;
        cursors[3] = r->Height;
        return atlas->TexPixelsAlpha8;
    }

    // A font's size, ascent and descent, and for a codepoint its advance and the glyph
    // drawn for it (codepoint, visible, x0 y0 x1 y1, u0 v0 u1 v1, advance).
    void cpp_font(int font, float* metrics)
    {
        metrics[0] = fonts[font]->FontSize;
        metrics[1] = fonts[font]->Ascent;
        metrics[2] = fonts[font]->Descent;
    }
    float cpp_glyph(int font, unsigned c, float* out)
    {
        ImFont* f = fonts[font];
        const ImFontGlyph* g = f->FindGlyph((ImWchar)c);
        out[0] = float(g->Codepoint);
        out[1] = g->Visible ? 1.0f : 0.0f;
        out[2] = g->X0; out[3] = g->Y0; out[4] = g->X1; out[5] = g->Y1;
        out[6] = g->U0; out[7] = g->V0; out[8] = g->U1; out[9] = g->V1;
        out[10] = g->AdvanceX;
        return (int)c < f->IndexAdvanceX.Size ? f->IndexAdvanceX.Data[c] : f->FallbackAdvanceX;
    }

    void cpp_begin(float width, float height)
    {
        ImGuiIO& io = ImGui::GetIO();
        io.DisplaySize = { width, height };
        io.DeltaTime = 1.0f / 60.0f;
        ImGui::NewFrame();
        list = ImGui::GetBackgroundDrawList();
    }

    // The frame rendered, and its background list as triangles: for every index, the
    // command's clip and texture and the vertex it names.
    const unsigned char* cpp_end(size_t* size)
    {
        ImGui::Render();
        flat.clear();
        for (const ImDrawCmd& cmd : list->CmdBuffer)
            for (unsigned i = 0; i < cmd.ElemCount; i++)
            {
                const ImDrawVert& v = list->VtxBuffer[int(cmd.VtxOffset + list->IdxBuffer[int(cmd.IdxOffset + i)])];
                const uint64_t texture = uint64_t(cmd.TextureId);
                unsigned char record[44];
                std::memcpy(record, &cmd.ClipRect, 16);
                std::memcpy(record + 16, &texture, 8);
                std::memcpy(record + 24, &v.pos, 8);
                std::memcpy(record + 32, &v.uv, 8);
                std::memcpy(record + 40, &v.col, 4);
                flat.insert(flat.end(), record, record + 44);
            }
        *size = flat.size();
        return flat.data();
    }

    // Dear ImGui's SDL renderer backend on `renderer`, as main.cpp started it, its font
    // atlas made into a picture of that renderer's; that picture.
    uint64_t cpp_backend_init(SDL_Renderer* renderer)
    {
        ImGui_ImplSDLRenderer3_Init(renderer);
        ImGui_ImplSDLRenderer3_NewFrame();
        return uint64_t(ImGui::GetIO().Fonts->TexID);
    }
    // The frame rendered last, drawn by the backend.
    void cpp_render(SDL_Renderer* renderer) { ImGui_ImplSDLRenderer3_RenderDrawData(ImGui::GetDrawData(), renderer); }

    void cpp_push_clip(const float* min, const float* max) { list->PushClipRect(P(min), P(max), true); }
    void cpp_pop_clip() { list->PopClipRect(); }
    void cpp_line(const float* a, const float* b, unsigned col, float thickness) { list->AddLine(P(a), P(b), col, thickness); }
    void cpp_rect(const float* a, const float* b, unsigned col, float rounding, float thickness) { list->AddRect(P(a), P(b), col, rounding, 0, thickness); }
    void cpp_rect_filled(const float* a, const float* b, unsigned col, float rounding) { list->AddRectFilled(P(a), P(b), col, rounding); }
    void cpp_multi(const float* a, const float* b, const unsigned* cols) { list->AddRectFilledMultiColor(P(a), P(b), cols[0], cols[1], cols[2], cols[3]); }
    void cpp_quad(const float* p, unsigned col) { list->AddQuadFilled(P(p), P(p + 2), P(p + 4), P(p + 6), col); }
    void cpp_circle(const float* c, float radius, unsigned col) { list->AddCircleFilled(P(c), radius, col); }
    void cpp_image(uint64_t texture, const float* a, const float* b, const float* uv0, const float* uv1, unsigned col)
    {
        list->AddImage((ImTextureID)texture, P(a), P(b), P(uv0), P(uv1), col);
    }
    void cpp_stroke(const float* p, int count, unsigned col, int closed, float thickness)
    {
        for (int i = 0; i < count; i++) list->PathLineTo(P(p + i * 2));
        list->PathStroke(col, closed ? ImDrawFlags_Closed : 0, thickness);
    }
    void cpp_poly(const float* p, int count, unsigned col)
    {
        std::vector<ImVec2> points;
        for (int i = 0; i < count; i++) points.push_back(P(p + i * 2));
        list->AddConvexPolyFilled(points.data(), count, col);
    }
    // Text as ui.cpp drew it: a C string, or (`one`) the single byte Spaced() drew at a time.
    void cpp_text(int font, float size, const float* at, unsigned col, const char* text, int one, float wrap)
    {
        list->AddText(fonts[font], size, P(at), col, text, one ? text + 1 : nullptr, wrap);
    }
    int cpp_text_size(int font, float size, float max_width, float wrap, const char* text, int one, float* out)
    {
        const char* remaining = nullptr;
        const ImVec2 measured = fonts[font]->CalcTextSizeA(size, max_width, wrap, text, one ? text + 1 : nullptr, &remaining);
        out[0] = measured.x;
        out[1] = measured.y;
        return int(remaining - text);
    }
}
