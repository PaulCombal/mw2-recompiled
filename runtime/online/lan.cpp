// Players on the same network, over plain UDP: the smallest real transport, and
// the one that tests the kernel side without an account anywhere. Two copies on
// one machine find each other too, which the title's own sockets cannot do --
// both would want port 1000.
//
// Each copy sends from a port of its own, and a peer is that port's address:
// the IPv4 address in the top bits, the port in the low 16. Broadcasts go to a
// port every copy shares (MW2_LAN_PORT, 3074 by default), which the kernel
// delivers to all of them; each copy's own port is the first free one after
// it, and how far after is the copy's slot.
#include "transport.h"
#include "udp.h"
#include "../env.h"
#include "../log.h"

#include <string>
#include <vector>

#ifdef _WIN32
#include <iphlpapi.h>
#else
#include <ifaddrs.h>
#include <net/if.h>
#endif

namespace
{
    using udp::Socket;
    using udp::kNoSocket;

    uint64_t IdOf(const sockaddr_in& address)
    {
        return (uint64_t(ntohl(address.sin_addr.s_addr)) << 16) | ntohs(address.sin_port);
    }

    sockaddr_in AddressOf(uint64_t id)
    {
        sockaddr_in address{};
        address.sin_family = AF_INET;
        address.sin_addr.s_addr = htonl(uint32_t(id >> 16));
        address.sin_port = htons(uint16_t(id));
        return address;
    }

    // The network connections that are up, the loopback aside, in the order
    // the system lists them.
    struct Adapter { std::string name; uint32_t address; };
    std::vector<Adapter> Adapters()
    {
        std::vector<Adapter> found;
#ifdef _WIN32
        ULONG size = 16 * 1024;
        std::vector<uint8_t> buffer(size);
        auto* adapters = reinterpret_cast<IP_ADAPTER_ADDRESSES*>(buffer.data());
        constexpr ULONG kFlags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        if (GetAdaptersAddresses(AF_INET, kFlags, nullptr, adapters, &size) == ERROR_BUFFER_OVERFLOW)
        {
            buffer.resize(size);
            adapters = reinterpret_cast<IP_ADAPTER_ADDRESSES*>(buffer.data());
        }
        if (GetAdaptersAddresses(AF_INET, kFlags, nullptr, adapters, &size) != NO_ERROR) return found;
        for (auto* at = adapters; at; at = at->Next)
        {
            if (at->OperStatus != IfOperStatusUp || at->IfType == IF_TYPE_SOFTWARE_LOOPBACK) continue;
            char name[128] = "?";
            if (at->FriendlyName)
                WideCharToMultiByte(CP_UTF8, 0, at->FriendlyName, -1, name, sizeof name, nullptr, nullptr);
            name[sizeof name - 1] = 0;
            for (auto* unicast = at->FirstUnicastAddress; unicast; unicast = unicast->Next)
            {
                const auto* address = reinterpret_cast<const sockaddr_in*>(unicast->Address.lpSockaddr);
                if (address->sin_family == AF_INET)
                    found.push_back({ name, ntohl(address->sin_addr.s_addr) });
            }
        }
#else
        ifaddrs* list = nullptr;
        if (getifaddrs(&list) != 0) return found;
        for (ifaddrs* at = list; at; at = at->ifa_next)
        {
            if (!at->ifa_addr || at->ifa_addr->sa_family != AF_INET) continue;
            if (!(at->ifa_flags & IFF_UP) || (at->ifa_flags & IFF_LOOPBACK)) continue;
            found.push_back({ at->ifa_name, ntohl(reinterpret_cast<sockaddr_in*>(at->ifa_addr)->sin_addr.s_addr) });
        }
        freeifaddrs(list);
#endif
        return found;
    }

    // The address other copies reach this one at: the first connection that is
    // up and not the loopback, or the loopback when there is none. The log
    // names them all: with more than one, the first may not be the one the
    // other players are on.
    uint32_t OwnAddress()
    {
        const std::vector<Adapter> adapters = Adapters();
        std::string list;
        for (const Adapter& adapter : adapters)
        {
            in_addr address;
            address.s_addr = htonl(adapter.address);
            char text[INET_ADDRSTRLEN];
            inet_ntop(AF_INET, &address, text, sizeof text);
            list += (list.empty() ? "" : ", ") + adapter.name + " " + text;
        }
        if (adapters.empty()) LOGI("online: no network connection is up; lan stays on this machine");
        else LOGI("online: network connections: %s; announcing the first", list.c_str());
        return adapters.empty() ? uint32_t(INADDR_LOOPBACK) : adapters.front().address;
    }

    class Lan final : public online::Transport
    {
    public:
        ~Lan() override
        {
            if (own_ != kNoSocket) udp::CloseSocket(own_);
            if (shared_ != kNoSocket) udp::CloseSocket(shared_);
        }

        bool Open() override
        {
            if (!udp::Startup()) { LOGE("online: no Winsock"); return false; }
            sharedPort_ = uint16_t(env::Number("MW2_LAN_PORT", 3074));
            own_ = udp::OpenOwnSocket(sharedPort_, slot_);
            shared_ = udp::OpenSocket(sharedPort_, true);
            if (own_ == kNoSocket || shared_ == kNoSocket)
            {
                LOGE("online: lan could not open its sockets (shared port %u)", sharedPort_);
                return false;
            }
            sockaddr_in bound{};
            socklen_t length = sizeof bound;
            getsockname(own_, reinterpret_cast<sockaddr*>(&bound), &length);
            bound.sin_addr.s_addr = htonl(OwnAddress());
            id_ = IdOf(bound);
            char text[INET_ADDRSTRLEN];
            inet_ntop(AF_INET, &bound.sin_addr, text, sizeof text);
            LOGI("online: lan at %s:%u, broadcasts on %u", text, ntohs(bound.sin_port), sharedPort_);
            return true;
        }

        const char* Name() const override { return "lan"; }
        uint64_t LocalId() const override { return id_; }
        uint32_t Slot() const override { return slot_; }

        bool SendTo(uint64_t peer, const void* bytes, size_t size) override
        {
            return udp::SendDatagram(own_, bytes, size, AddressOf(peer));
        }

        bool SendAll(const void* bytes, size_t size) override
        {
            sockaddr_in everyone{};
            everyone.sin_family = AF_INET;
            everyone.sin_addr.s_addr = htonl(INADDR_BROADCAST);
            everyone.sin_port = htons(sharedPort_);
            return udp::SendDatagram(own_, bytes, size, everyone);
        }

        long Next(void* bytes, size_t size, uint64_t& from) override
        {
            for (Socket s : { own_, shared_ })
            {
                sockaddr_in address{};
                const long got = udp::ReceiveDatagram(s, bytes, size, address);
                if (got < 0) continue;
                from = IdOf(address);
                return got;
            }
            return -1;
        }

    private:
        Socket own_ = kNoSocket, shared_ = kNoSocket;
        uint16_t sharedPort_ = 0;
        uint64_t id_ = 0;
        uint32_t slot_ = 0;
    };
}

std::unique_ptr<online::Transport> online::MakeLanTransport() { return std::make_unique<Lan>(); }
std::unique_ptr<online::Service> online::MakeLan() { return MakePlayers(MakeLanTransport()); }
