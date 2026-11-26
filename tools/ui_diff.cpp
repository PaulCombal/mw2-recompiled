// tools/ui_diff.py's C++ side: the launcher's window as it was drawn with Dear ImGui 1.91.9b
// (launcher/ui.cpp and preview.cpp, compiled from git), with main.cpp's frame -- its IO,
// its SDL3 backend fed SDL's events, its renderer backend -- around them. ui_diff.rs calls
// the same of its Rust (launcher/src/ui.rs, preview.rs, input.rs) and compares what each
// made.
#include "ui.h"
#include "imgui.h"
#include "imgui_impl_sdl3.h"
#include "imgui_impl_sdlrenderer3.h"

#include <SDL3/SDL.h>

#include <cstdint>
#include <cstring>
#include <string>
#include <vector>

namespace
{
    ui::Fonts fonts;
    ui::Backdrop backdrop;
    std::vector<unsigned char> flat;

    // The frame, as ui_diff.rs writes it.
    struct Reader
    {
        const unsigned char* at;
        uint32_t u32()
        {
            uint32_t v;
            std::memcpy(&v, at, 4);
            at += 4;
            return v;
        }
        int i32() { return int(u32()); }
        float f32()
        {
            float v;
            std::memcpy(&v, at, 4);
            at += 4;
            return v;
        }
        bool flag() { return *at++ != 0; }
        uint64_t u64()
        {
            uint64_t v;
            std::memcpy(&v, at, 8);
            at += 8;
            return v;
        }
        std::string text()
        {
            const uint32_t n = u32();
            std::string s(reinterpret_cast<const char*>(at), n);
            at += n;
            return s;
        }
    };

    ui::Frame Decode(const unsigned char* data)
    {
        Reader r{ data };
        ui::Frame f;
        const uint32_t entries = r.u32();
        for (uint32_t i = 0; i < entries; i++)
        {
            ui::Entry e;
            e.label = r.text();
            e.enabled = r.flag();
            e.ruleAbove = r.flag();
            e.tag = r.text();
            f.entries.push_back(e);
        }
        f.focus = r.i32();
        f.heading = r.text();
        f.text = r.text();
        f.error = r.flag();
        f.busy = r.flag();
        f.step = r.i32();
        f.steps = r.i32();
        f.stepTitle = r.text();
        f.detail = r.text();
        f.amount = r.text();
        f.fraction = r.f32();
        const uint32_t cards = r.u32();
        for (uint32_t i = 0; i < cards; i++)
        {
            ui::Frame::Card c;
            c.emblem = ui::Frame::Emblem(r.i32());
            c.name = r.text();
            c.title = r.text();
            c.label = r.text();
            f.cards.push_back(c);
        }
        f.slider = r.f32();
        f.fov = r.f32();
        f.aspect = r.f32();
        f.picture = reinterpret_cast<SDL_Texture*>(uintptr_t(r.u64()));
        f.status = r.text();
        f.corner = r.text();
        f.hint = r.text();
        return f;
    }

    const ImGuiKey kAsked[] = { ImGuiKey_LeftArrow, ImGuiKey_RightArrow, ImGuiKey_UpArrow, ImGuiKey_DownArrow,
                                ImGuiKey_Enter, ImGuiKey_KeypadEnter, ImGuiKey_Space, ImGuiKey_Escape,
                                ImGuiKey_GamepadDpadLeft, ImGuiKey_GamepadDpadRight, ImGuiKey_GamepadDpadUp,
                                ImGuiKey_GamepadDpadDown, ImGuiKey_GamepadLStickLeft, ImGuiKey_GamepadLStickRight,
                                ImGuiKey_GamepadLStickUp, ImGuiKey_GamepadLStickDown, ImGuiKey_GamepadFaceDown,
                                ImGuiKey_GamepadFaceRight, ImGuiKey_A, ImGuiKey_Comma, ImGuiKey_Keypad5 };
}

extern "C"
{
    // ---- the drawing -----------------------------------------------------------------
    // A context with the fonts as ui::LoadFonts loads them at `scale`.
    void cpp_init(float scale)
    {
        ImGui::CreateContext();
        ImGuiIO& io = ImGui::GetIO();
        io.IniFilename = nullptr;
        fonts = ui::LoadFonts(scale);
        unsigned char* rgba;
        int width, height;
        io.Fonts->GetTexDataAsRGBA32(&rgba, &width, &height);
        io.Fonts->SetTexID((ImTextureID)1);
    }

    // The backdrop's Steam emblem as a picture named `id`, for frames drawn without a renderer.
    void cpp_set_steam(uint64_t id) { backdrop.steam = reinterpret_cast<SDL_Texture*>(uintptr_t(id)); }

    // A frame begun with the pointer at x, y (or nowhere), its button down or not, `dt`
    // seconds after the last; what Dear ImGui's IO then says: the pointer's place and
    // movement, whether the button went down, is down, the time and the step.
    void cpp_begin(float width, float height, int nowhere, float x, float y, int down, float dt, double* io_out)
    {
        ImGuiIO& io = ImGui::GetIO();
        io.DisplaySize = { width, height };
        io.DeltaTime = dt;
        if (nowhere) io.AddMousePosEvent(-FLT_MAX, -FLT_MAX);
        else io.AddMousePosEvent(x, y);
        io.AddMouseButtonEvent(0, down != 0);
        ImGui::NewFrame();
        io_out[0] = io.MousePos.x;
        io_out[1] = io.MousePos.y;
        io_out[2] = io.MouseDelta.x;
        io_out[3] = io.MouseDelta.y;
        io_out[4] = ImGui::IsMouseClicked(ImGuiMouseButton_Left);
        io_out[5] = ImGui::IsMouseDown(ImGuiMouseButton_Left);
        io_out[6] = ImGui::GetTime();
        io_out[7] = io.DeltaTime;
    }

    // ui::Draw of the frame `data` describes; the entry chosen, and `focus` and `slid` as it left them.
    int cpp_draw(const unsigned char* data, float scale, int* focus, float* slid)
    {
        const ui::Frame frame = Decode(data);
        return ui::Draw(fonts, backdrop, frame, scale, *focus, *slid);
    }

    // The frame rendered, and its background list as triangles (draw_diff.cpp's records).
    const unsigned char* cpp_end(size_t* size)
    {
        ImGui::Render();
        ImDrawList* list = ImGui::GetBackgroundDrawList();
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

    // ---- the pictures ----------------------------------------------------------------
    // The backdrop made on `renderer` and Dear ImGui's renderer backend started on it;
    // the backdrop's Steam emblem.
    uint64_t cpp_backdrop(SDL_Renderer* renderer)
    {
        backdrop = ui::MakeBackdrop(renderer);
        ImGui_ImplSDLRenderer3_Init(renderer);
        ImGui_ImplSDLRenderer3_NewFrame();
        return uint64_t(uintptr_t(backdrop.steam));
    }
    // main.cpp's picture of a frame: the clear, the backdrop `seconds` in, the list.
    void cpp_render(SDL_Renderer* renderer, double seconds)
    {
        SDL_SetRenderDrawColor(renderer, 40, 40, 38, 255);
        SDL_RenderClear(renderer);
        ui::DrawBackdrop(renderer, backdrop, seconds);
        ImGui_ImplSDLRenderer3_RenderDrawData(ImGui::GetDrawData(), renderer);
    }

    // ---- the input ---------------------------------------------------------------------
    // Dear ImGui's SDL3 backend on `window`, with main.cpp's settings.
    void cpp_input_init(SDL_Window* window, SDL_Renderer* renderer)
    {
        ImGui::CreateContext();
        ImGuiIO& io = ImGui::GetIO();
        io.IniFilename = nullptr;
        io.ConfigFlags |= ImGuiConfigFlags_NavEnableKeyboard | ImGuiConfigFlags_NavEnableGamepad;
        io.KeyRepeatRate = 0.10f;
        ImGui_ImplSDL3_InitForSDLRenderer(window, renderer);
        unsigned char* rgba;
        int width, height;
        io.Fonts->GetTexDataAsRGBA32(&rgba, &width, &height);
    }
    void cpp_event(const SDL_Event* event) { ImGui_ImplSDL3_ProcessEvent(event); }
    // A frame `dt` seconds after the last (the backend's own step replaced by it); what
    // the window asks of it: each asked key pressed, and pressed or repeated, the pointer,
    // the time, the window's size and pixels.
    void cpp_input_frame(float dt, int* keys, double* io_out)
    {
        ImGui_ImplSDL3_NewFrame();
        ImGuiIO& io = ImGui::GetIO();
        io.DeltaTime = dt;
        ImGui::NewFrame();
        int n = 0;
        for (ImGuiKey key : kAsked)
        {
            keys[n++] = ImGui::IsKeyPressed(key, false);
            keys[n++] = ImGui::IsKeyPressed(key, true);
        }
        io_out[0] = io.MousePos.x;
        io_out[1] = io.MousePos.y;
        io_out[2] = io.MouseDelta.x;
        io_out[3] = io.MouseDelta.y;
        io_out[4] = ImGui::IsMouseClicked(ImGuiMouseButton_Left);
        io_out[5] = ImGui::IsMouseDown(ImGuiMouseButton_Left);
        io_out[6] = ImGui::GetTime();
        io_out[7] = io.DeltaTime;
        io_out[8] = io.DisplaySize.x;
        io_out[9] = io.DisplaySize.y;
        io_out[10] = io.DisplayFramebufferScale.x;
        io_out[11] = io.DisplayFramebufferScale.y;
        ImGui::EndFrame();
    }
}
