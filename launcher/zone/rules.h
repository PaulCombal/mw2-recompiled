#pragma once
// The loading rules the launcher carries (CMake embeds defs/ and defs_pc/),
// read once: the 360 title update's, which the shader pass and the map
// converter read the game's files by, and the PC game's, which the map
// converter reads a PC map by.
#include "schema.h"

namespace zone
{
    const Schema& XboxRules();
    const Schema& PcRules();

    // An embedded definition file by name ("pc/..." for the PC's).
    std::string Definition(const std::string& name);
}
