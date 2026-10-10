#include "preview.h"

#include <imgui.h>

#include <algorithm>
#include <cmath>
#include <vector>

namespace
{
    constexpr float kRadians = 0.01745329f;
    constexpr float kAspect = 16.0f / 9.0f;
    constexpr float kEye = 60;      // the game's units, an inch each: where a standing player's eyes are
    constexpr float kNear = 6;

    // The game's axes: x ahead, y to the left, z up.
    struct Point { float x, y, z; };
    struct Colour { float r, g, b; };
    struct Face
    {
        Point corner[4];
        Point normal;
        Colour colour;
        bool flat = false;          // painted on the ground: under everything else
        bool held = false;          // in the player's hands: over everything else
    };

    constexpr Colour kHaze{ 168, 170, 160 };

    void Box(std::vector<Face>& faces, Point low, Point high, Colour colour)
    {
        const float x0 = low.x, y0 = low.y, z0 = low.z, x1 = high.x, y1 = high.y, z1 = high.z;
        faces.push_back({ { { x0, y0, z0 }, { x0, y1, z0 }, { x0, y1, z1 }, { x0, y0, z1 } }, { -1, 0, 0 }, colour });
        faces.push_back({ { { x1, y0, z0 }, { x1, y1, z0 }, { x1, y1, z1 }, { x1, y0, z1 } }, { 1, 0, 0 }, colour });
        faces.push_back({ { { x0, y0, z0 }, { x1, y0, z0 }, { x1, y0, z1 }, { x0, y0, z1 } }, { 0, -1, 0 }, colour });
        faces.push_back({ { { x0, y1, z0 }, { x1, y1, z0 }, { x1, y1, z1 }, { x0, y1, z1 } }, { 0, 1, 0 }, colour });
        faces.push_back({ { { x0, y0, z1 }, { x1, y0, z1 }, { x1, y1, z1 }, { x0, y1, z1 } }, { 0, 0, 1 }, colour });
    }

    void Paint(std::vector<Face>& faces, float x0, float x1, float y0, float y1, Colour colour)
    {
        Face face{ { { x0, y0, 0 }, { x1, y0, 0 }, { x1, y1, 0 }, { x0, y1, 0 } }, { 0, 0, 1 }, colour };
        face.flat = true;
        faces.push_back(face);
    }

    // A figure the height of a soldier: something whose size on the picture
    // says how far it is.
    void Soldier(std::vector<Face>& faces, float x, float y)
    {
        const Colour cloth{ 82, 88, 62 }, skin{ 150, 120, 96 };
        Box(faces, { x - 5, y - 9, 0 }, { x + 5, y - 1, 32 }, cloth);
        Box(faces, { x - 5, y + 1, 0 }, { x + 5, y + 9, 32 }, cloth);
        Box(faces, { x - 6, y - 11, 32 }, { x + 6, y + 11, 58 }, cloth);
        Box(faces, { x - 5, y - 5, 59 }, { x + 5, y + 5, 70 }, skin);
    }

    // The weapon and the hands on it, a few inches ahead of the eye and below
    // it to the right. The game draws its own with the view's angle like
    // everything else, so a wider view makes it smaller and longer.
    void Weapon(std::vector<Face>& faces)
    {
        const size_t first = faces.size();
        const Colour steel{ 46, 47, 50 }, dark{ 30, 31, 33 }, glove{ 58, 60, 50 }, sleeve{ 82, 88, 62 };
        const float e = kEye;
        Box(faces, { 6.5f, -6.4f, e - 7.6f }, { 10, -4.6f, e - 4.8f }, dark);           // the stock
        Box(faces, { 10, -6.6f, e - 8 }, { 22, -4.4f, e - 4.5f }, steel);              // the receiver
        Box(faces, { 22, -6.3f, e - 7.2f }, { 34, -4.7f, e - 5 }, dark);               // the handguard
        Box(faces, { 34, -5.8f, e - 6.3f }, { 41, -5.2f, e - 5.7f }, steel);           // the barrel
        Box(faces, { 11, -5.8f, e - 4.5f }, { 12, -5.2f, e - 3.7f }, dark);            // the sights
        Box(faces, { 33, -5.7f, e - 5 }, { 33.6f, -5.3f, e - 3.8f }, dark);
        Box(faces, { 17, -6, e - 12.5f }, { 19.5f, -5, e - 8 }, dark);                 // the magazine
        Box(faces, { 9, -7.4f, e - 11.5f }, { 14, -4.2f, e - 8 }, glove);              // the hand on the grip
        Box(faces, { 6.5f, -10.5f, e - 16 }, { 10.5f, -6.2f, e - 10 }, sleeve);
        Box(faces, { 24, -7, e - 9.6f }, { 29, -4.2f, e - 7.2f }, glove);              // the hand under the handguard
        Box(faces, { 15, -4.6f, e - 14 }, { 25, 0.5f, e - 9.6f }, sleeve);
        for (size_t i = first; i < faces.size(); i++) faces[i].held = true;
    }

    std::vector<Face> Street()
    {
        std::vector<Face> faces;
        // The road, its kerbs and the line down its middle.
        Paint(faces, kNear, 6000, -210, 210, { 62, 62, 60 });
        Paint(faces, kNear, 6000, 200, 210, { 120, 120, 112 });
        Paint(faces, kNear, 6000, -210, -200, { 120, 120, 112 });
        for (float x = 60; x < 4000; x += 220) Paint(faces, x, x + 110, -4, 4, { 176, 170, 130 });

        // Buildings down both sides, with a gap for a side street.
        const Colour wall[] = { { 128, 112, 94 }, { 112, 104, 96 }, { 140, 126, 104 }, { 98, 96, 92 } };
        const struct { float from, to, height; int colour; } left[] = {
            { -200, 520, 300, 0 }, { 560, 1300, 430, 1 }, { 1560, 2400, 340, 2 }, { 2440, 3600, 520, 3 }, { 3640, 5200, 380, 0 } };
        const struct { float from, to, height; int colour; } right[] = {
            { -200, 300, 360, 2 }, { 620, 1500, 280, 3 }, { 1540, 2100, 460, 0 }, { 2380, 3400, 330, 1 }, { 3440, 5200, 480, 2 } };
        for (const auto& b : left) Box(faces, { b.from, 260, 0 }, { b.to, 700, b.height }, wall[b.colour]);
        for (const auto& b : right) Box(faces, { b.from, -700, 0 }, { b.to, -260, b.height }, wall[b.colour]);
        Box(faces, { 5200, -900, 0 }, { 5600, 900, 600 }, wall[3]);

        // Beside the player, where only a wide view reaches: a wall to the
        // left, crates to the right.
        Box(faces, { 20, 150, 0 }, { 150, 162, 96 }, { 150, 144, 128 });
        Box(faces, { 70, -150, 0 }, { 110, -110, 40 }, { 122, 96, 58 });
        Box(faces, { 78, -144, 40 }, { 104, -118, 66 }, { 134, 106, 64 });

        // Ahead: a car's worth of box, a barrier, and soldiers at three distances.
        Box(faces, { 620, 60, 0 }, { 800, 140, 52 }, { 70, 84, 104 });
        Box(faces, { 650, 68, 52 }, { 760, 132, 78 }, { 58, 70, 88 });
        Box(faces, { 1150, -190, 0 }, { 1170, -40, 44 }, { 150, 150, 142 });
        Soldier(faces, 330, -60);
        Soldier(faces, 900, 30);
        Soldier(faces, 1900, -50);
        Weapon(faces);
        return faces;
    }

    struct Seen { float x, y, z; };     // right, up, ahead of the eye

    Seen FromEye(const Point& p) { return { -p.y, p.z - kEye, p.x }; }

    ImU32 Shade(Colour colour, float light, float distance)
    {
        const float haze = 1 - std::exp(-distance / 3200);
        auto channel = [&](float c, float h) { return int(std::clamp(c * light + (h - c * light) * haze, 0.0f, 255.0f)); };
        return IM_COL32(channel(colour.r, kHaze.r), channel(colour.g, kHaze.g), channel(colour.b, kHaze.b), 255);
    }
}

float preview::Across(float fov)
{
    return 2 * std::atan(std::tan(fov * 0.5f * kRadians) * 0.75f * kAspect) / kRadians;
}

void preview::Draw(ImDrawList* draw, const ImVec2& min, const ImVec2& max, float fov)
{
    static const std::vector<Face> street = Street();
    static float shown = fov;
    shown += (fov - shown) * std::min(1.0f, ImGui::GetIO().DeltaTime * 9);
    if (std::abs(fov - shown) < 0.02f) shown = fov;

    // The game's own: the angle's tangent is the 4:3 picture's half width,
    // three quarters of it the half height, and the width follows the shape.
    const float tanY = std::tan(shown * 0.5f * kRadians) * 0.75f, tanX = tanY * kAspect;
    const ImVec2 middle{ (min.x + max.x) * 0.5f, (min.y + max.y) * 0.5f };
    const float halfWidth = (max.x - min.x) * 0.5f, halfHeight = (max.y - min.y) * 0.5f;
    auto onPicture = [&](const Seen& s) {
        return ImVec2{ middle.x + s.x / s.z / tanX * halfWidth, middle.y - s.y / s.z / tanY * halfHeight };
    };

    draw->PushClipRect(min, max, true);
    // The sky, and the ground to the horizon, which a level view has across the middle.
    draw->AddRectFilledMultiColor(min, { max.x, middle.y }, IM_COL32(104, 122, 138, 255), IM_COL32(104, 122, 138, 255),
                                  IM_COL32(168, 170, 160, 255), IM_COL32(168, 170, 160, 255));
    draw->AddRectFilledMultiColor({ min.x, middle.y }, max, IM_COL32(150, 150, 140, 255), IM_COL32(150, 150, 140, 255),
                                  IM_COL32(84, 80, 70, 255), IM_COL32(84, 80, 70, 255));

    struct Drawn { ImVec2 corner[8]; int corners; float distance; ImU32 colour; int layer; };
    std::vector<Drawn> drawn;
    drawn.reserve(street.size());
    const Point sun{ -0.45f, -0.35f, 0.82f };
    for (const Face& face : street)
    {
        // Faces turned away from the eye are behind their box.
        const Point& c = face.corner[0];
        if (face.normal.x * c.x + face.normal.y * c.y + face.normal.z * (c.z - kEye) >= 0) continue;

        // Cut at the near plane: what is behind the eye has no place on the picture.
        Seen cut[8];
        int count = 0;
        for (int i = 0; i < 4; i++)
        {
            const Seen a = FromEye(face.corner[i]), b = FromEye(face.corner[(i + 1) % 4]);
            if (a.z >= kNear) cut[count++] = a;
            if ((a.z >= kNear) != (b.z >= kNear))
            {
                const float t = (kNear - a.z) / (b.z - a.z);
                cut[count++] = { a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t, kNear };
            }
        }
        if (count < 3) continue;

        Drawn d;
        d.corners = count;
        d.layer = face.flat ? 0 : face.held ? 2 : 1;
        float sum = 0;
        for (int i = 0; i < count; i++)
        {
            d.corner[i] = onPicture(cut[i]);
            sum += std::sqrt(cut[i].x * cut[i].x + cut[i].y * cut[i].y + cut[i].z * cut[i].z);
        }
        d.distance = sum / float(count);
        // The draw list smooths the edge of a shape whose corners go clockwise.
        float turn = 0;
        for (int i = 0; i < count; i++)
        {
            const ImVec2& a = d.corner[i], & b = d.corner[(i + 1) % count];
            turn += a.x * b.y - b.x * a.y;
        }
        if (turn < 0) std::reverse(d.corner, d.corner + count);
        const float lit = std::max(0.0f, face.normal.x * sun.x + face.normal.y * sun.y + face.normal.z * sun.z);
        // The ground runs from the feet to the horizon: its haze is the far end's, not the middle's.
        d.colour = Shade(face.colour, 0.58f + 0.42f * lit, face.flat ? 500 : d.distance);
        drawn.push_back(d);
    }
    // The ground, the street, then what the player holds; in each the farthest
    // first. The boxes do not cross, so nearer ones cover them rightly.
    std::stable_sort(drawn.begin(), drawn.end(), [](const Drawn& a, const Drawn& b) {
        return a.layer != b.layer ? a.layer < b.layer : a.distance > b.distance;
    });
    for (const Drawn& d : drawn) draw->AddConvexPolyFilled(d.corner, d.corners, d.colour);

    // Where the player aims.
    const float tick = std::max(2.0f, halfHeight * 0.035f);
    for (int side = -1; side <= 1; side += 2)
    {
        draw->AddLine({ middle.x + side * tick, middle.y }, { middle.x + side * tick * 2.6f, middle.y }, IM_COL32(236, 236, 232, 220), 1.5f);
        draw->AddLine({ middle.x, middle.y + side * tick }, { middle.x, middle.y + side * tick * 2.6f }, IM_COL32(236, 236, 232, 220), 1.5f);
    }
    draw->PopClipRect();
    draw->AddRect(min, max, IM_COL32(0, 0, 0, 200));
}
