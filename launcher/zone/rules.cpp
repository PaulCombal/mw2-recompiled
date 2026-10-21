#include "rules.h"

#include <memory>
#include <mutex>

// defs/ and defs_pc/, as CMake embeds them (launcher_zone_defs.cpp).
struct ZoneDefinition
{
    const char* name;
    const unsigned char* data;
    unsigned size;
};
extern const ZoneDefinition kZoneDefinitions[];
extern const unsigned kZoneDefinitionCount;

namespace zone
{
    std::string Definition(const std::string& name)
    {
        for (unsigned k = 0; k < kZoneDefinitionCount; k++)
            if (name == kZoneDefinitions[k].name)
                return std::string(reinterpret_cast<const char*>(kZoneDefinitions[k].data), kZoneDefinitions[k].size);
        throw Error("the launcher has no definition file " + name);
    }

    namespace
    {
        std::unique_ptr<Schema> Load(const std::string& prefix)
        {
            // The rules include "XAssets/<asset>.txt"; they are in assets/ here.
            return std::make_unique<Schema>(Definition(prefix + "iw4_assets.h"), Definition(prefix + "commands.txt"),
                                            [&](const std::string& name) {
                                                const size_t slash = name.rfind('/');
                                                return Definition(prefix + "assets/" + name.substr(slash + 1));
                                            });
        }
    }

    const Schema& XboxRules()
    {
        static std::once_flag once;
        static std::unique_ptr<Schema> schema;
        std::call_once(once, [] { schema = Load(""); });
        return *schema;
    }

    const Schema& PcRules()
    {
        static std::once_flag once;
        static std::unique_ptr<Schema> schema;
        std::call_once(once, [] { schema = Load("pc/"); });
        return *schema;
    }
}
