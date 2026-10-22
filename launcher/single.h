#pragma once
// One launcher window a copy of the game: a second start of the window brings
// the first one up in front and ends. The copy is the launcher's folder, so
// two installs side by side each have their own.
//
// Linux has an abstract socket named after the folder, which ends with the
// process that holds it; Windows a named mutex and an event the second start
// sets. A window brought up is one the system allows in front: Windows lets
// the second start hand its place over, X11 raises it, and a Wayland
// compositor may only mark it as asking for attention.
namespace single
{
    // Whether this start is the window of its copy. `takeOver`: it is the one
    // an update started (update.h), and waits for the one it replaces to end.
    bool Claim(bool takeOver);
    // Asks the window that has the copy to come up in front.
    void Wake();
    // Whether a second start asked since the last look.
    bool Woken();
}
