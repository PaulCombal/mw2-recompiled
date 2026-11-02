#include "single.h"

#include <SDL3/SDL.h>

#ifdef _WIN32
#include <windows.h>
#else
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#ifdef __APPLE__
#include <fcntl.h>
#include <sys/stat.h>
#endif
#endif
#include <cctype>
#include <cerrno>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <thread>

namespace
{
    // How long the launcher an update started waits for the old one to end:
    // it ends as soon as it has started the new one.
    constexpr auto kTakeOver = std::chrono::seconds(5);

    // The copy's name: its folder, hashed (FNV-1a).
    std::string Name()
    {
        std::string folder = SDL_GetBasePath() ? SDL_GetBasePath() : "";
#ifdef _WIN32
        for (char& c : folder) c = char(std::tolower(static_cast<unsigned char>(c)));
#endif
        uint64_t hash = 0xcbf29ce484222325ull;
        for (unsigned char c : folder) hash = (hash ^ c) * 0x100000001b3ull;
        char name[40];
        std::snprintf(name, sizeof(name), "mw2-launcher-%016llx", static_cast<unsigned long long>(hash));
        return name;
    }

#ifdef _WIN32
    HANDLE g_mutex = nullptr, g_wake = nullptr;
    bool g_claimed = false;

    HANDLE Wakeup()
    {
        const std::string name = "Local\\" + Name() + "-wake";
        return CreateEventA(nullptr, FALSE, FALSE, name.c_str());
    }
#else
    int g_socket = -1;

#ifdef __APPLE__
    // macOS has no abstract namespace: the socket is a file in the temporary
    // folder, which a launcher that was killed leaves behind (Claim takes it over).
    socklen_t Address(sockaddr_un& address)
    {
        const char* folder = std::getenv("TMPDIR");
        const std::string path = std::string(folder && *folder ? folder : "/tmp") + (folder && *folder && folder[std::strlen(folder) - 1] == '/' ? "" : "/") + Name() + ".sock";
        address = {};
        address.sun_family = AF_UNIX;
        std::snprintf(address.sun_path, sizeof(address.sun_path), "%s", path.c_str());
        return socklen_t(offsetof(sockaddr_un, sun_path) + std::strlen(address.sun_path) + 1);
    }

    int NewSocket(bool nonBlocking)
    {
        const int s = socket(AF_UNIX, SOCK_STREAM, 0);
        if (s < 0) return s;
        fcntl(s, F_SETFD, FD_CLOEXEC);
        if (nonBlocking) fcntl(s, F_SETFL, fcntl(s, F_GETFL) | O_NONBLOCK);
        return s;
    }

    int Accept(int listening)
    {
        const int c = accept(listening, nullptr, nullptr);
        if (c >= 0) fcntl(c, F_SETFD, FD_CLOEXEC);
        return c;
    }
#else
    // In the abstract namespace: no file, and gone with its process.
    socklen_t Address(sockaddr_un& address)
    {
        const std::string name = Name();
        address = {};
        address.sun_family = AF_UNIX;
        std::memcpy(address.sun_path + 1, name.data(), name.size());
        return socklen_t(offsetof(sockaddr_un, sun_path) + 1 + name.size());
    }

    int NewSocket(bool nonBlocking)
    {
        return socket(AF_UNIX, SOCK_STREAM | (nonBlocking ? SOCK_NONBLOCK : 0) | SOCK_CLOEXEC, 0);
    }

    int Accept(int listening) { return accept4(listening, nullptr, nullptr, SOCK_CLOEXEC); }
#endif
#endif
}

bool single::Claim(bool takeOver)
{
#ifdef _WIN32
    const std::string name = "Local\\" + Name();
    g_wake = Wakeup();
    g_mutex = CreateMutexA(nullptr, FALSE, name.c_str());
    if (!g_mutex) return false;
    const DWORD waited = WaitForSingleObject(g_mutex, takeOver ? DWORD(std::chrono::milliseconds(kTakeOver).count()) : 0);
    g_claimed = waited == WAIT_OBJECT_0 || waited == WAIT_ABANDONED;
    return g_claimed;
#else
    sockaddr_un address;
    const socklen_t length = Address(address);
    const auto until = std::chrono::steady_clock::now() + kTakeOver;
    for (;;)
    {
        const int s = NewSocket(true);
        if (s < 0) return false;
        if (bind(s, reinterpret_cast<const sockaddr*>(&address), length) == 0 && listen(s, 4) == 0)
        {
            g_socket = s;
            return true;
        }
#ifdef __APPLE__
        // The file of a launcher that is no longer there: nothing answers it.
        if (errno == EADDRINUSE)
        {
            const int probe = NewSocket(false);
            const bool alive = probe >= 0 && connect(probe, reinterpret_cast<const sockaddr*>(&address), length) == 0;
            if (probe >= 0) close(probe);
            if (!alive && unlink(address.sun_path) == 0)
            {
                close(s);
                continue;
            }
        }
#endif
        close(s);
        if (!takeOver || std::chrono::steady_clock::now() > until) return false;
        std::this_thread::sleep_for(std::chrono::milliseconds(50));
    }
#endif
}

void single::Wake()
{
#ifdef _WIN32
    // Only the program in front may put another there, and a start the
    // player made is: it hands the place over.
    AllowSetForegroundWindow(ASFW_ANY);
    if (!g_wake) g_wake = Wakeup();
    if (g_wake) SetEvent(g_wake);
#else
    // The connection is the message.
    sockaddr_un address;
    const socklen_t length = Address(address);
    const int s = NewSocket(false);
    if (s < 0) return;
    connect(s, reinterpret_cast<const sockaddr*>(&address), length);
    close(s);
#endif
}

bool single::Woken()
{
#ifdef _WIN32
    return g_claimed && g_wake && WaitForSingleObject(g_wake, 0) == WAIT_OBJECT_0;
#else
    bool woken = false;
    for (int c; g_socket >= 0 && (c = Accept(g_socket)) >= 0; close(c)) woken = true;
    return woken;
#endif
}
