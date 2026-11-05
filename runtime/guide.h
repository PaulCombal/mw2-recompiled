#pragma once
// The Guide button's screen for the first controller, as the console's guide
// opened over any game: who is playing, the time, what the title says the
// player is doing (presence.h), the network and the friends on it, then
// Resume, Invite friends (friends.h) and Quit to desktop. The keyboard's Home
// key is that button. The first controller drives it; the others wait until
// it shuts. At the other controllers the button opens the sign-in screen
// (signin.h), as before.
#include "screen.h"

#include <cstdint>

extern "C"
{
    bool mw2_guide_input(uint32_t user, uint16_t buttons);
    bool mw2_guide_picture(uint32_t windowHeight, mw2_image* image);
}

namespace guide
{
    // What a controller holds, at every read of it, as signin::Input takes
    // it. True when the title is not to see it.
    inline bool Input(uint32_t user, uint16_t buttons) { return mw2_guide_input(user, buttons); }

    // The screen, for a window of this height. False while it is shut.
    inline bool Picture(uint32_t windowHeight, screen::Image& image)
    {
        return screen::Picture(mw2_guide_picture, windowHeight, image);
    }
}
