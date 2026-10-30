#pragma once
// How the players a service reaches exchange packets: the local network's UDP
// (lan.cpp) or a relay server everyone connects to (server.cpp). players.cpp
// builds the whole service on one -- the title's datagrams, who is there and
// what can be joined -- so both have the same System Link list and the same
// Xbox LIVE parties and private matches.
#include "service.h"

#include <cstddef>
#include <cstdint>
#include <memory>

namespace online
{
    class Transport
    {
    public:
        virtual ~Transport() = default;

        // Opens it, and logs why when it cannot.
        virtual bool Open() = 0;
        // "lan", "server": for the log.
        virtual const char* Name() const = 0;
        // Where the others reach this copy, stable for the run.
        virtual uint64_t LocalId() const = 0;
        // Which copy on this machine this is: 0 for the first to start, so a
        // second copy is a second player.
        virtual uint32_t Slot() const = 0;

        virtual bool SendTo(uint64_t peer, const void* bytes, size_t size) = 0;
        // To everyone it reaches; it may come back to this copy too.
        virtual bool SendAll(const void* bytes, size_t size) = 0;
        // The next packet that arrived and who from, without waiting: its
        // size, or -1 when nothing is waiting.
        virtual long Next(void* bytes, size_t size, uint64_t& from) = 0;

        // Optional, as on Service: what only a server everyone shares can do.
        virtual bool Matchmaking() { return false; }
        virtual void List(const SessionListing* listing) { (void)listing; }
        virtual uint32_t Search(const SessionSearch& search) { (void)search; return 0; }
        virtual bool SearchResults(uint32_t search, std::vector<SessionFound>& out)
        {
            (void)search; (void)out;
            return false;
        }
        virtual bool TitleFile(const std::string& name, std::vector<uint8_t>& out)
        {
            (void)name; (void)out;
            return false;
        }
    };

    // The service over a transport (players.cpp).
    std::unique_ptr<Service> MakePlayers(std::unique_ptr<Transport> transport);

    // The transports, each in its own file.
    std::unique_ptr<Transport> MakeLanTransport();
    std::unique_ptr<Transport> MakeServerTransport();
}
