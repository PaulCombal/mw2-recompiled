#include "version.h"

#ifndef MW2_RELEASE_TAG
#define MW2_RELEASE_TAG ""
#endif

extern "C" const char* mw2_version_release() { return MW2_RELEASE_TAG; }

extern "C" uint16_t mw2_version_network()
{
    // FNV-1a of the tag, folded to 15 bits: the title prints its protocol,
    // which carries this, as a signed number.
    const char* tag = *MW2_RELEASE_TAG ? MW2_RELEASE_TAG : "dev";
    uint32_t hash = 2166136261u;
    for (const char* c = tag; *c; ++c) hash = (hash ^ uint8_t(*c)) * 16777619u;
    const uint16_t folded = uint16_t((hash ^ (hash >> 15) ^ (hash >> 30)) & 0x7FFF);
    return folded ? folded : 1;
}
