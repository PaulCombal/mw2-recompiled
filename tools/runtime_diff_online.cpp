// tools/runtime_diff.py's way to the C++ online service's pure parts, which live in
// anonymous namespaces and a class's private members: its files, included here, with
// what the rig calls them by. Built twice, once with RIG_STEAM for steam.cpp and once
// without for service.cpp, as their anonymous namespaces would meet in one file.
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <map>
#include <string>

#ifdef RIG_STEAM
// Every header steam.cpp includes, first, so that only its own class is opened up.
#include "../runtime/online/service.h"
#include "../runtime/env.h"
#include "../runtime/log.h"
#include "../runtime/platform.h"
#include "../runtime/signin.h"
#include "../runtime/version.h"
#include <algorithm>
#include <atomic>
#include <cctype>
#include <cerrno>
#include <chrono>
#include <csignal>
#include <cstdio>
#include <cstdlib>
#include <deque>
#include <fcntl.h>
#include <filesystem>
#include <fstream>
#include <mutex>
#include <sched.h>
#include <set>
#include <string_view>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <thread>
#include <unistd.h>
#include <vector>
#define private public
#include "../runtime/online/steam.cpp"
#undef private

// Steam's own: a listing's text read into a session found and its numbers.
extern "C" bool rigcpp_read_game(const char* text, uint64_t* counts, uint8_t* session, uint32_t* numbers, size_t* count)
{
    online::SessionFound found;
    std::map<uint32_t, uint32_t> read;
    const bool ok = Steam::ReadGame(text, found, read);
    counts[0] = found.openPublic;
    counts[1] = found.openPrivate;
    counts[2] = found.filledPublic;
    counts[3] = found.filledPrivate;
    std::memcpy(session, found.session, sizeof found.session);
    *count = 0;
    for (const auto& [id, value] : read)
    {
        numbers[2 * *count] = id;
        numbers[2 * *count + 1] = value;
        ++*count;
    }
    return ok;
}

extern "C" bool rigcpp_read_session(const char* hex, uint8_t* session) { return Steam::ReadSession(hex, session); }

extern "C" void rigcpp_elsewhere(const char* playing, char* out, size_t room)
{
    std::snprintf(out, room, "%s", Elsewhere(playing).c_str());
}
#else
// The rig's own service stands in for Steam (runtime_diff.cpp).
#define MakeSteam MakeRigSteam
#define MakeLan MakeRigLan
#include "../runtime/online/service.cpp"
#undef MakeSteam
#undef MakeLan

// What service.cpp makes, which the rig never starts (steam.cpp's are in the other build).
std::unique_ptr<online::Backend> online::MakeServer() { return nullptr; }
void kernel::FinishSearches() {}
void kernel::AcceptInvite(uint64_t, const uint8_t*, bool) {}

extern "C" bool rigcpp_is_connect_line(const char* line, size_t size) { return IsConnectLine(std::string(line, size)); }
#endif
