// MW2_RENDER=<w>x<h>: the frame the title draws, in place of the console's
// 1280x720, and so the picture's shape (render_size.h).
//
// The title takes its sizes from no video mode: R_VideoSizes writes the
// console's own, a 1024x600 scene and a 1280x720 frame, whenever the mode is
// high definition, and R_SetVidConfig gives the picture an aspect of 16/9 or
// 4/3 by the mode's widescreen flag alone. Everything after reads the
// vidConfig it makes: D3D's back buffer and render targets are made at its
// sizes, and the views' projections, the HUD's and the menus' placement and
// the cinematics' letterbox follow its aspect -- the PC game's engine, which
// draws at any shape. So the sizes are written after R_VideoSizes, and the
// aspect after R_SetVidConfig: the frame's width over its height, square
// pixels, and the scene's pixel aspect from it. A cinematic is letterboxed by
// the title itself, whole between black bars.
//
// A 2D picture made for the console's 16:9 screen and drawn over the whole
// frame -- a loading screen, a menu's background, a full-screen overlay --
// covers the frame instead of stretching to it: it is as large as the frame
// where the shapes differ, and what spills over is cut, as a 16:9 picture on a
// 16:10 screen is cut at its sides. It is one whose every texture was loaded
// from a file: a copy of the screen (a render target), which the menus blur
// behind them, is drawn over the frame too, and must be left whole.
//
// The menus lay some things out over the screen's width in their own units,
// which they take as 640 + 213.333 * dvarbool(widescreen): 640 for 4:3 and
// 853.33 for 16:9, the console's two shapes -- the experience bar's width,
// for one, which ran off a 16:10 screen. A constant a menu expression
// multiplies dvarbool(widescreen) by is scaled by the share of the way from
// 4:3 to 16:9 the picture's shape is (UI_EvaluateStatement): 0.6 at 16:10,
// which makes that width 768, the 16:10 screen's. dvarbool(widescreen) itself
// stays 1: the menus also test it, the loading screen's bar for one, and a
// test truncates what it is given to a whole number.
//
// With the console's frame none of this does anything.
#include <ppc_recomp_shared.h>
#include "title.h"
#include "render_size.h"
#include "env.h"
#include "log.h"

#include <cctype>
#include <cstring>
#include <mutex>
#include <set>
#include <string>
#include <unordered_map>

namespace
{
    constexpr float kConsoleAspect = 16.0f / 9.0f;

    // The vidConfig's fields
    constexpr uint32_t kSceneWidth = 0, kSceneHeight = 4, kFrameWidth = 8, kFrameHeight = 12;
    constexpr uint32_t kAspect = 28, kScenePixelAspect = 32, kFramePixelAspect = 36;
    // R_VideoSizes's
    constexpr uint32_t kHiDef = 1, kScene = 8, kFrame = 16;
    // A Material's and a GfxImage's, on the 360 (launcher/zone/defs/iw4_assets.h)
    constexpr uint32_t kMaterialTextureCount = 57, kMaterialTextureTable = 68;
    constexpr uint32_t kTextureDefSize = 12, kTextureDefImage = 8;
    constexpr uint32_t kImageCategory = 58, kLoadedFromFile = 3;   // IMG_CATEGORY_LOAD_FROM_FILE
    // R_DrawStretchPic's material, past its eight floats, in the caller's
    // parameter area
    constexpr uint32_t kStretchPicMaterial = 92;
    // An expression's Statement_s, its ExpressionSupportingData's static dvar
    // list, a StaticDvar { dvar_t*, name }, and an Operand { dataType, value }
    constexpr uint32_t kStatementSupportingData = 8, kSupportingStaticDvars = 12, kStaticDvarName = 4;
    constexpr uint32_t kOperandInt = 0, kOperandFloat = 1;

    float AsFloat(uint32_t bits) { float f; std::memcpy(&f, &bits, 4); return f; }
    uint32_t Bits(float f) { uint32_t u; std::memcpy(&u, &f, 4); return u; }

#ifdef T_R_VideoSizes
    // Whether every texture of `material` was loaded from a file: a picture,
    // not a copy of the screen.
    bool Loaded(uint8_t* base, uint32_t material)
    {
        const uint32_t count = PPC_LOAD_U8(material + kMaterialTextureCount);
        const uint32_t table = PPC_LOAD_U32(material + kMaterialTextureTable);
        if (!count || !table) return false;
        for (uint32_t i = 0; i < count; i++)
        {
            const uint32_t image = PPC_LOAD_U32(table + i * kTextureDefSize + kTextureDefImage);
            if (!image || PPC_LOAD_U8(image + kImageCategory) != kLoadedFromFile) return false;
        }
        return true;
    }

    // Says once a material which picture was made to cover the frame.
    void Say(uint8_t* base, uint32_t material)
    {
        static std::mutex lock;
        static std::set<uint32_t> said;
        std::lock_guard hold(lock);
        if (said.size() >= 64 || !said.insert(material).second) return;
        const uint32_t at = PPC_LOAD_U32(material);
        const char* name = at ? reinterpret_cast<const char*>(base + at) : "unnamed";
        LOGI("render: a picture covers the frame (%s)", std::string(name, strnlen(name, 64)).c_str());
    }

    // Cuts a 16:9 picture's coordinates to the part of it a frame of
    // `aspect` shows when the picture covers it.
    void Cover(PPCContext& ctx, float aspect)
    {
        auto cut = [](PPCRegister& from, PPCRegister& to, float kept) {
            const double middle = (from.f64 + to.f64) * 0.5;
            from.f64 = double(float(middle + (from.f64 - middle) * kept));
            to.f64 = double(float(middle + (to.f64 - middle) * kept));
        };
        if (aspect < kConsoleAspect) cut(ctx.f5, ctx.f7, aspect / kConsoleAspect);
        else if (aspect > kConsoleAspect) cut(ctx.f6, ctx.f8, kConsoleAspect / aspect);
    }

    bool NamedWidescreen(const char* name)
    {
        static constexpr char kName[] = "widescreen";
        for (size_t i = 0; i < sizeof kName; i++)
            if (std::tolower(static_cast<unsigned char>(name[i])) != kName[i]) return false;
        return true;
    }

    // The statement's entries, { type, op or Operand }, 12 bytes each: an
    // operator (type 0) or an operand (type 1, an Operand { dataType, value }).
    constexpr uint32_t kStatementEntryCount = 0, kStatementEntries = 4, kEntrySize = 12;
    constexpr uint32_t kOperator = 0;
    constexpr uint32_t kOpRightParen = 1, kOpMultiply = 2, kOpStaticDvarBool = 24;

    // Whether entry `at` is dvarbool(static) of `widescreen`: the operator, its
    // index into the statement's static dvar list, and the closing paren.
    bool IsWidescreenTest(uint8_t* base, uint32_t statement, uint32_t entries, uint32_t count, uint32_t at)
    {
        auto entry = [&](uint32_t i) { return entries + i * kEntrySize; };
        if (at + 2 >= count || PPC_LOAD_U32(entry(at)) != kOperator ||
            PPC_LOAD_U32(entry(at) + 4) != kOpStaticDvarBool || PPC_LOAD_U32(entry(at + 1)) == kOperator ||
            PPC_LOAD_U32(entry(at + 1) + 4) != kOperandInt)
            return false;
        const uint32_t list = PPC_LOAD_U32(PPC_LOAD_U32(statement + kStatementSupportingData) + kSupportingStaticDvars);
        const uint32_t static_ = PPC_LOAD_U32(list + 4 * PPC_LOAD_U32(entry(at + 1) + 8));
        const uint32_t name = static_ ? PPC_LOAD_U32(static_ + kStaticDvarName) : 0;
        return name && NamedWidescreen(reinterpret_cast<const char*>(base + name));
    }

    // Scales the constants the statement multiplies dvarbool(widescreen) by:
    // `c * dvarbool(widescreen)` and `dvarbool(widescreen) * c`.
    void ScaleWidescreenTerms(uint8_t* base, uint32_t statement, float share)
    {
        static std::mutex lock;
        static std::unordered_map<uint32_t, uint32_t> made;   // a constant's entry, and the bits it was given
        const uint32_t count = PPC_LOAD_U32(statement + kStatementEntryCount);
        const uint32_t entries = PPC_LOAD_U32(statement + kStatementEntries);
        auto entry = [&](uint32_t i) { return entries + i * kEntrySize; };
        auto isOp = [&](uint32_t i, uint32_t op) {
            return i < count && PPC_LOAD_U32(entry(i)) == kOperator && PPC_LOAD_U32(entry(i) + 4) == op;
        };
        auto scale = [&](uint32_t i) {
            const uint32_t at = entry(i);
            if (PPC_LOAD_U32(at) == kOperator) return;
            const uint32_t type = PPC_LOAD_U32(at + 4), bits = PPC_LOAD_U32(at + 8);
            if (type != kOperandInt && type != kOperandFloat) return;
            std::lock_guard hold(lock);
            if (auto kept = made.find(at); kept != made.end() && kept->second == bits && type == kOperandFloat) return;
            const float value = type == kOperandFloat ? AsFloat(bits) : float(int32_t(bits));
            const uint32_t scaled = Bits(value * share);
            PPC_STORE_U32(at + 4, kOperandFloat);
            PPC_STORE_U32(at + 8, scaled);
            made[at] = scaled;
        };
        for (uint32_t i = 0; i < count; i++)
        {
            if (!IsWidescreenTest(base, statement, entries, count, i)) continue;
            if (i >= 2 && isOp(i - 1, kOpMultiply)) scale(i - 2);
            if (isOp(i + 2, kOpRightParen) && isOp(i + 3, kOpMultiply) && i + 4 < count) scale(i + 4);
        }
    }

    // How far the picture's shape is from 4:3 towards 16:9: 1 at 16:9. A frame
    // no wider than 4:3 leaves the constants be: dvarbool(widescreen) is 0
    // then, the video mode's (render_size.h).
    float Widescreen()
    {
        static const float share = [] {
            const render::Size frame = render::Frame();
            if (!render::Widescreen(frame)) return 1.0f;
            const float aspect = float(frame.width) / float(frame.height);
            return (aspect - 4.0f / 3.0f) / (kConsoleAspect - 4.0f / 3.0f);
        }();
        return share;
    }
#endif
}

render::Size render::Frame()
{
    static const Size frame = [] {
        const char* asked = env::Text("MW2_RENDER");
        if (!asked) return kConsoleFrame;
        const std::optional<Size> size = Parse(asked);
        if (!size || !FrameValid(*size))
        {
            LOGW("render: MW2_RENDER=%s is not a size the title can draw at (640x360 to 1280x800, "
                 "a whole number of 16 pixels wide and 4 tall); it draws the console's 1280x720", asked);
            return kConsoleFrame;
        }
#ifndef T_R_VideoSizes
        LOGW("render: the " MW2_TITLE_NAME " cannot be given another size than the console's; "
             "MW2_RENDER=%s is left out", asked);
        return kConsoleFrame;
#else
        if (*size != kConsoleFrame)
        {
            const Size scene = Scene(*size);
            LOGI("render: the title draws a %ux%u frame and a %ux%u scene (MW2_RENDER)",
                 size->width, size->height, scene.width, scene.height);
        }
        return *size;
#endif
    }();
    return frame;
}

#ifdef T_R_VideoSizes

GUEST_HOOK(T_R_VideoSizes)
{
    const uint32_t config = ctx.r3.u32;
    GUEST_ORIG(T_R_VideoSizes)(ctx, base);
    const render::Size frame = render::Frame();
    if (frame == render::kConsoleFrame || !PPC_LOAD_U8(config + kHiDef)) return;
    const render::Size scene = render::Scene(frame);
    PPC_STORE_U32(config + kScene, scene.width);
    PPC_STORE_U32(config + kScene + 4, scene.height);
    PPC_STORE_U32(config + kFrame, frame.width);
    PPC_STORE_U32(config + kFrame + 4, frame.height);
}

GUEST_HOOK(T_R_SetVidConfig)
{
    GUEST_ORIG(T_R_SetVidConfig)(ctx, base);
    if (render::Frame() == render::kConsoleFrame) return;
    // As the title works them out, in single precision.
    const uint32_t config = T_DATA_VidConfig;
    const float sceneWidth = float(PPC_LOAD_U32(config + kSceneWidth));
    const float sceneHeight = float(PPC_LOAD_U32(config + kSceneHeight));
    const float frameWidth = float(PPC_LOAD_U32(config + kFrameWidth));
    const float frameHeight = float(PPC_LOAD_U32(config + kFrameHeight));
    const float aspect = frameWidth / frameHeight;
    PPC_STORE_U32(config + kAspect, Bits(aspect));
    PPC_STORE_U32(config + kScenePixelAspect, Bits(sceneHeight * aspect / sceneWidth));
    PPC_STORE_U32(config + kFramePixelAspect, Bits(frameHeight * aspect / frameWidth));
}

GUEST_HOOK(T_R_DrawStretchPic)
{
    if (render::Frame() != render::kConsoleFrame)
    {
        const uint32_t config = T_DATA_VidConfig;
        const double width = double(PPC_LOAD_U32(config + kFrameWidth));
        const double height = double(PPC_LOAD_U32(config + kFrameHeight));
        const float aspect = AsFloat(PPC_LOAD_U32(config + kAspect));
        const uint32_t material = PPC_LOAD_U32(ctx.r1.u32 + kStretchPicMaterial);
        if (material && ctx.f1.f64 <= 0.5 && ctx.f2.f64 <= 0.5 &&
                 ctx.f1.f64 + ctx.f3.f64 >= width - 0.5 && ctx.f2.f64 + ctx.f4.f64 >= height - 0.5 &&
                 Loaded(base, material))
        {
            Cover(ctx, aspect);
            Say(base, material);
        }
    }
    GUEST_ORIG(T_R_DrawStretchPic)(ctx, base);
}

// UI_EvaluateStatement(localClientNum, statement): a menu expression's value.
// Before it runs, a constant it multiplies dvarbool(widescreen) by is scaled
// by the shape's share. Each constant is scaled once: one that holds what it
// was made is left, and one a zone loaded again over it is made again.
GUEST_HOOK(T_UI_EvaluateStatement)
{
    const float share = Widescreen();
    if (share != 1.0f) ScaleWidescreenTerms(base, ctx.r4.u32, share);
    GUEST_ORIG(T_UI_EvaluateStatement)(ctx, base);
}

#endif
