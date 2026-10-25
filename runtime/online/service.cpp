#include "service.h"
#include "../console.h"
#include "../env.h"
#include "../kernel/kernel.h"
#include "../log.h"

#include <cstring>

namespace
{
    // A backend and those tried after it when it does not start.
    struct Backend
    {
        const char* name;
        std::unique_ptr<online::Service> (*make)();
    };
    constexpr Backend kLan[] = { { "lan", online::MakeLan }, { nullptr, nullptr } };
    constexpr Backend kServer[] = { { "server", online::MakeServer }, { nullptr, nullptr } };
    constexpr Backend kSteam[] = { { "steam", online::MakeSteam }, { "lan", online::MakeLan }, { nullptr, nullptr } };
    constexpr Backend kNone[] = { { nullptr, nullptr } };

    // MW2_NETWORK: what the player chose; none when unset or unknown.
    const Backend* Chosen()
    {
        const char* network = env::Text("MW2_NETWORK");
        if (!network || !*network || std::strcmp(network, "none") == 0) return kNone;
        if (std::strcmp(network, "lan") == 0) return kLan;
        if (std::strcmp(network, "server") == 0) return kServer;
        if (std::strcmp(network, "steam") == 0) return kSteam;
        LOGW("online: MW2_NETWORK=%s is none of none, lan, server, steam; playing without a network", network);
        return kNone;
    }
}

bool online::EnterServiceNamespace(int& exitCode)
{
    if (Chosen() != kSteam) return true;
    return EnterSteamNamespace(exitCode);
}

online::Service* online::Get()
{
    static Service* const service = [] {
        static std::unique_ptr<Service> made;
        const Backend* const chosen = Chosen();
        for (const Backend* backend = chosen; backend->make; backend++)
        {
            made = backend->make();
            if (made->Start())
            {
                LOGI("online: the service is %s", backend->name);
                return made.get();
            }
            made.reset();
            LOGW("online: %s did not start", backend->name);
        }
        if (chosen->make) LOGW("online: no service started; the title stays offline");
        else LOGI("online: no network chosen; system link stays on this machine");
        return static_cast<Service*>(nullptr);
    }();
    return service;
}

bool online::Live()
{
    static const bool live = [] {
        auto* service = Get();
        return service && service->Lobbies();
    }();
    return live;
}

// What the System Link menu runs when the player opens it, before its server
// list types the host's connect line. Connect loads the player's stats if they
// are not loaded, but only once it is connecting, which the title refuses.
//
// A player signed in to Live has the Live stats loaded from the start, and the
// title asks such a player which stats to play system link with. The answer
// here is the Live ones: they are there already, and the rank is then the same
// in a private match and in system link.
namespace
{
    constexpr const char* kSystemLinkMenu[] = {
        "xblive_rankedmatch 0", "xblive_privatematch 0", "exec default_systemlink.cfg",
        "exec default_720p.cfg", "exec dvar_defaults.cfg", "xrequiresignin",
    };
    constexpr const char* kUseLiveStats = "set useonlinestats 1";

    // Whether a line a service hands over is the one a host advertises
    // (kernel's AdvertiseHosted): "connect", the XNADDR, key and session id in
    // hex, the two slot counts, the nonce in hex, a space between each and
    // nothing else. The line goes to the title's console, which runs whatever
    // follows a ';' or a newline as a command of its own, and on Steam it is
    // lobby data the lobby's owner wrote: anything of another shape is refused.
    bool IsConnectLine(const std::string& line)
    {
        // hex digits wanted per token; 0 is a decimal number of up to ten digits
        constexpr size_t kTokens[] = { 72, 32, 16, 0, 0, 16 };
        constexpr char kCommand[] = "connect";
        if (line.compare(0, sizeof kCommand - 1, kCommand) != 0) return false;
        size_t at = sizeof kCommand - 1;
        for (const size_t hex : kTokens)
        {
            if (at >= line.size() || line[at++] != ' ') return false;
            size_t digits = 0;
            for (; at < line.size() && line[at] != ' '; at++, digits++)
            {
                const char c = line[at];
                const bool decimal = c >= '0' && c <= '9';
                if (!decimal && !(hex && ((c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F')))) return false;
            }
            if (hex ? digits != hex : (digits < 1 || digits > 10)) return false;
        }
        return at == line.size();
    }
}

void online::Frame()
{
    auto* service = Get();
    if (!service) return;
    Invite invite;
    if (service->NextInvite(invite))
    {
        LOGI("online: joining a friend's Xbox LIVE %s", invite.fromInvite ? "invitation" : "session");
        kernel::AcceptInvite(invite.inviter, invite.session, invite.fromInvite);
    }
    std::string connect;
    if (!service->NextJoin(connect)) return;
    if (!IsConnectLine(connect))
    {
        LOGW("online: a friend's game was advertised with a line that is not a connect command; not joining it");
        return;
    }
    LOGI("online: joining a friend's game");
    if (Live()) console::RunNow(kUseLiveStats);
    for (const char* command : kSystemLinkMenu) console::RunNow(command);
    console::RunNow(connect.c_str());
}
