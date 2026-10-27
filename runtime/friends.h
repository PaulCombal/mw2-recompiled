#pragma once
// The screen that invites friends. On the console the title asks for the
// system's friend list (XamShowFriendsUI) and never draws one; this is that
// list, over the frame as the sign-in screen is (signin.h): the friends the
// online service sees running this game, with their pictures, then how many
// others it does not. A invites the one chosen, B goes back.
//
// A service with no friends to pick from (online::Service::Friends) gets a
// message instead, and invites everyone it can as it did before: LAN's.
#include "screen.h"

#include <cstdint>

namespace friends
{
    // Opens the screen, from the title's lobby or from F6.
    void Ask();

    // What a controller holds, at every read of it, as signin::Input takes
    // it. True when the title is not to see it.
    bool Input(uint32_t user, uint16_t buttons);

    // The screen, for a window of this height. False while it is shut.
    bool Picture(uint32_t windowHeight, screen::Image& image);
}
