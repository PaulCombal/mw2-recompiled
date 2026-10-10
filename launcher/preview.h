#pragma once
// What a field of view looks like: a street drawn by hand, from where a player
// stands, with the game's own projection -- the angle is the width of a 4:3
// picture and the 16:9 one is as tall and wider. It is a drawing, not the
// game's picture: it shows what an angle does, more to the sides and
// everything ahead smaller.
struct ImDrawList;
struct ImVec2;

namespace preview
{
    // Degrees across the 16:9 picture for the game's angle.
    float Across(float fov);

    // Draws the street in the rectangle, at the angle. The angle shown moves
    // to a new one over a moment, so the change is seen happening.
    void Draw(ImDrawList* draw, const ImVec2& min, const ImVec2& max, float fov);
}
