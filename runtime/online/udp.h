#pragma once
// The few places the two socket APIs differ, for the transports that use the
// host's UDP (lan.cpp, server.cpp).
#include <cstddef>
#include <cstdint>
#include <cstdlib>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <winsock2.h>
#include <ws2tcpip.h>
#else
#include <arpa/inet.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>
#endif

namespace udp
{
#ifdef _WIN32
    using Socket = SOCKET;
    inline const Socket kNoSocket = INVALID_SOCKET;
    inline void CloseSocket(Socket s) { closesocket(s); }
    inline void MakeNonBlocking(Socket s) { u_long on = 1; ioctlsocket(s, FIONBIO, &on); }
    inline bool Startup() { WSADATA wsa; return WSAStartup(MAKEWORD(2, 2), &wsa) == 0; }
#else
    using Socket = int;
    inline const Socket kNoSocket = -1;
    inline void CloseSocket(Socket s) { ::close(s); }
    inline void MakeNonBlocking(Socket s) { fcntl(s, F_SETFL, fcntl(s, F_GETFL, 0) | O_NONBLOCK); }
    inline bool Startup() { return true; }
#endif

    inline bool SendDatagram(Socket s, const void* data, size_t size, const sockaddr_in& to)
    {
        return ::sendto(s, static_cast<const char*>(data), int(size), 0,
                        reinterpret_cast<const sockaddr*>(&to), sizeof to) >= 0;
    }

    // -1 when nothing is waiting.
    inline long ReceiveDatagram(Socket s, void* data, size_t size, sockaddr_in& from)
    {
        socklen_t length = sizeof from;
        return long(::recvfrom(s, static_cast<char*>(data), int(size), 0,
                               reinterpret_cast<sockaddr*>(&from), &length));
    }

    // A non-blocking socket on `port` of every address; broadcasts allowed,
    // and the port shared with other copies when `shared`.
    inline Socket OpenSocket(uint16_t port, bool shared)
    {
        Socket s = ::socket(AF_INET, SOCK_DGRAM, 0);
        if (s == kNoSocket) return kNoSocket;
        int on = 1;
        setsockopt(s, SOL_SOCKET, SO_BROADCAST, reinterpret_cast<const char*>(&on), sizeof on);
        if (shared) setsockopt(s, SOL_SOCKET, SO_REUSEADDR, reinterpret_cast<const char*>(&on), sizeof on);
        MakeNonBlocking(s);
        sockaddr_in address{};
        address.sin_family = AF_INET;
        address.sin_addr.s_addr = htonl(INADDR_ANY);
        address.sin_port = htons(port);
        if (::bind(s, reinterpret_cast<sockaddr*>(&address), sizeof address) != 0)
        {
            CloseSocket(s);
            return kNoSocket;
        }
        return s;
    }

    // A port of a copy's own, the first free one above `base`; `slot` is how
    // far above, 0 for the first copy on this machine to start.
    inline Socket OpenOwnSocket(uint16_t base, uint32_t& slot)
    {
        for (slot = 0; slot < 64; slot++)
            if (Socket s = OpenSocket(uint16_t(base + 1 + slot), false); s != kNoSocket) return s;
        return kNoSocket;
    }
}
