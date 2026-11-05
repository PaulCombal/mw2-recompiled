#pragma once
// What the title says each player is doing, the line the console's guide and a
// friend's list showed: "Playing Team Deathmatch in Afghan".
//
// The title sets contexts (XUserSetContext): the presence mode under
// X_CONTEXT_PRESENCE, and the values its modes' strings name, such as the map.
// Its XLAST, the Live configuration gzipped into the SPA resource of its image
// (title.h's T_DATA_Spa), says what each mode and value reads as: a mode is a
// string with {c<id>} where a context's value goes. English, as XGetLanguage
// says.
#include <cstdint>
#include <string>
#include "text_sink.h"

extern "C"
{
    void mw2_presence_set_context(uint32_t user, uint32_t id, uint32_t value);
    void mw2_presence_text(uint32_t user, void* line, mw2_text_sink sink);
}

namespace presence
{
    inline void SetContext(uint32_t user, uint32_t id, uint32_t value) { mw2_presence_set_context(user, id, value); }

    // The line for the player at this controller; empty before the title set
    // a mode, or where the image has no XLAST.
    inline std::string Text(uint32_t user)
    {
        std::string line;
        mw2_presence_text(user, &line, mw2_assign_text);
        return line;
    }
}
