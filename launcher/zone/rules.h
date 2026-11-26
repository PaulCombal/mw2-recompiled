#pragma once
// The loading rules the map converter carries (CMake embeds zone/defs/ and
// zone/defs_pc/), read once: the 360 title update's, which it reads the
// game's files by, and the PC game's, which it reads a PC map by. (The
// compile pass reads the 360's with the zone crate, zone/src/.)
#include "schema.h"

namespace zone
{
    const Schema& XboxRules();
    const Schema& PcRules();

    // An embedded definition file by name ("pc/..." for the PC's).
    std::string Definition(const std::string& name);
}
