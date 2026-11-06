// MW2_CAPTURE_GPU and the command processor's side of MW2_REPLAY_GPU (stream_capture.h).
// Empty in a release build, which does not even link it (build.rs, CMakeLists.txt).
#include "../diagnostics.h"

#if MW2_DIAGNOSTICS

#include "stream_capture.h"
#include "internal.h"
#include "memory_watch.h"
#include "../crash.h"
#include "../guest.h"
#include "../image_move.h"
#include "../log.h"
#include "../kernel/physical.h"

#include <algorithm>
#include <atomic>
#include <condition_variable>
#include <cstdarg>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <filesystem>
#include <mutex>
#include <string>
#include <thread>
#include <unordered_map>
#include <utility>
#include <vector>

namespace gpu::stream::detail
{
    bool capturing = false;
    bool replaying = false;
    bool counting = false;
}

namespace
{
    using namespace gpu::detail;
    using gpu::stream::detail::capturing;
    using gpu::stream::detail::counting;
    using gpu::stream::detail::replaying;

    constexpr uint32_t kFormatVersion = 2;
    constexpr uint32_t kPageBytes = 4096;
    constexpr uint32_t kPages = 1u << 20;                       // the whole guest space
    constexpr uint32_t kBankFirst = guest::kPhysicalBase >> 12;  // the physical bank's pages
    constexpr uint32_t kBankPages = (guest::kPhysicalEnd - guest::kPhysicalBase) >> 12;
    constexpr uint32_t kApertureFirst = kRegisterAperture >> 12;
    constexpr uint32_t kApertureLast = (kRegisterAperture + gpu::kRegisterCount * 4 - 1) >> 12;
    // What the capture holds for a page whose bytes the replay cannot know without
    // being given them: no page hashes to it.
    constexpr uint64_t kUnknown = 0;

    // A page's hash: content-addressing the blobs and telling a page that changed.
    // Four lanes of xxHash64's round, which keeps up with memcpy.
    uint64_t PageHash(const uint8_t* bytes)
    {
        constexpr uint64_t k1 = 0x9E3779B185EBCA87ull, k2 = 0xC2B2AE3D27D4EB4Full;
        constexpr uint64_t k3 = 0x165667B19E3779F9ull;
        const auto rotl = [](uint64_t v, int r) { return (v << r) | (v >> (64 - r)); };
        uint64_t lane[4] = { k1 + k2, k2, 0, 0 - k1 };
        for (uint32_t at = 0; at < kPageBytes; at += 32)
            for (uint32_t i = 0; i < 4; i++)
            {
                uint64_t word;
                std::memcpy(&word, bytes + at + i * 8, 8);
                lane[i] = rotl(lane[i] + word * k2, 31) * k1;
            }
        uint64_t h = rotl(lane[0], 1) + rotl(lane[1], 7) + rotl(lane[2], 12) + rotl(lane[3], 18);
        for (uint64_t v : lane) h = (h ^ (rotl(v * k2, 31) * k1)) * k1 + k3;
        h ^= h >> 33; h *= k2; h ^= h >> 29; h *= k3; h ^= h >> 32;
        return h == kUnknown ? 1 : h;
    }

    uint32_t Normalised(uint32_t address)
    {
        // The second window of the physical bank is the same bytes as the first.
        return address >= guest::kApertureBase ? address - (guest::kApertureBase - guest::kPhysicalBase)
                                               : address;
    }

    // ---- the capture's files, written by a thread of their own

    enum File { kIndexFile, kRingFile, kBlobFile, kBlobIndexFile, kStateFile, kFiles };
    constexpr const char* kFileNames[kFiles] = { "capture.idx", "ring.bin", "pages.blob", "pages.idx",
                                                 "state.bin" };

    class Writer
    {
    public:
        bool Open(const std::string& dir)
        {
            std::error_code error;
            std::filesystem::create_directories(dir, error);
            for (uint32_t f = 0; f < kFiles; f++)
            {
                files_[f] = std::fopen((dir + "/" + kFileNames[f]).c_str(), "wb");
                if (!files_[f])
                {
                    LOGW("capture: cannot write %s/%s", dir.c_str(), kFileNames[f]);
                    Close();
                    return false;
                }
            }
            thread_ = std::thread([this] { Run(); });
            return true;
        }

        // Waits while 256 MB are queued: the snapshot is most of physical memory.
        void Push(File file, std::vector<uint8_t>&& bytes)
        {
            if (bytes.empty()) return;
            std::unique_lock<std::mutex> hold(lock_);
            room_.wait(hold, [&] { return queued_ < (256u << 20); });
            queued_ += bytes.size();
            queue_.emplace_back(file, std::move(bytes));
            work_.notify_one();
        }
        void Push(File file, const std::string& text)
        {
            Push(file, std::vector<uint8_t>(text.begin(), text.end()));
        }

        void Close()
        {
            if (thread_.joinable())
            {
                {
                    std::lock_guard<std::mutex> hold(lock_);
                    closing_ = true;
                }
                work_.notify_one();
                thread_.join();
            }
            for (std::FILE*& f : files_)
                if (f) { std::fclose(f); f = nullptr; }
        }

        bool Failed() const { return failed_; }

    private:
        void Run()
        {
            crash::RegisterThread("gpu capture writer");
            std::unique_lock<std::mutex> hold(lock_);
            for (;;)
            {
                work_.wait(hold, [&] { return closing_ || !queue_.empty(); });
                if (queue_.empty()) return;
                auto [file, bytes] = std::move(queue_.front());
                queue_.pop_front();
                hold.unlock();
                if (std::fwrite(bytes.data(), 1, bytes.size(), files_[file]) != bytes.size()) failed_ = true;
                hold.lock();
                queued_ -= bytes.size();
                room_.notify_all();
            }
        }

        std::FILE* files_[kFiles]{};
        std::thread thread_;
        std::mutex lock_;
        std::condition_variable work_, room_;
        std::deque<std::pair<File, std::vector<uint8_t>>> queue_;
        size_t queued_ = 0;
        bool closing_ = false;
        std::atomic<bool> failed_{ false };
    };

    // ---- the capture, all of it the ring consumer's

    enum PageFlags : uint8_t
    {
        kDirty = 1,        // the consumer wrote it: looked at again at its next read
        kOwnPending = 2,   // and the replay will hold what it holds now
    };

    struct Capture
    {
        bool read = false, wanted = false, started = false;
        std::string dir;
        uint64_t after = 0, presents = 0;
        Writer writer;

        // Per page of the guest space: the hash of what the replay will hold there,
        // the batch it was last looked at in (+1), the flags above; per page of the
        // bank, the watch's count of writes it has accounted for.
        std::vector<uint64_t> held;
        std::vector<uint32_t> examined;
        std::vector<uint8_t> flags, writable;
        std::vector<uint32_t> seenLoose;
        uint64_t zeroHash = 0;
        uint32_t ringFirst = 0, ringLast = 0;   // the ring's pages: its dwords are kept apart

        std::unordered_map<uint64_t, uint32_t> blobs;
        std::vector<uint8_t> blobBuffer;
        std::string blobIndex;

        uint32_t batch = 0, packet = 0;
        std::string lines;                // the batch's page and event lines
        uint32_t pageLines = 0, eventLines = 0, ringDwords = 0;

        std::vector<uint8_t> apertureAtStart;
        std::unordered_map<uint32_t, uint32_t> aperture;
        uint32_t arenaBlock = 0;

        uint64_t totalRing = 0, totalVersions = 0, totalOwn = 0, totalEvents = 0;
        uint64_t pool = 0, waits = 0, waitTimeouts = 0, apertureReads = 0, arenaBlocks = 0;
        uint64_t snapshotPages = 0, touched = 0;
    };
    Capture c;

    void ReadSwitch()
    {
        if (c.read) return;
        c.read = true;
        const char* text = diag::Text("MW2_CAPTURE_GPU");
        if (!text || !*text) return;
        const std::string all = text;
        const size_t second = all.rfind(':');
        const size_t first = second == std::string::npos || second == 0 ? std::string::npos
                                                                         : all.rfind(':', second - 1);
        if (first == std::string::npos)
        {
            LOGW("capture: MW2_CAPTURE_GPU=%s is not <dir>:<after presents>:<presents>", text);
            return;
        }
        c.dir = all.substr(0, first);
        c.after = std::strtoull(all.c_str() + first + 1, nullptr, 10);
        c.presents = std::strtoull(all.c_str() + second + 1, nullptr, 10);
        c.wanted = !c.dir.empty() && c.presents > 0;
        if (c.wanted)
            LOGI("capture: the GPU's input from swap packet %llu for %llu more into %s",
                 (unsigned long long)c.after, (unsigned long long)c.presents, c.dir.c_str());
    }

    void Line(const char* format, ...) __attribute__((format(printf, 1, 2)));
    void Line(const char* format, ...)
    {
        char text[160];
        va_list arguments;
        va_start(arguments, format);
        std::vsnprintf(text, sizeof text, format, arguments);
        va_end(arguments);
        c.lines += text;
    }

    uint32_t Blob(const uint8_t* bytes, uint64_t hash)
    {
        const auto [found, inserted] = c.blobs.try_emplace(hash, uint32_t(c.blobs.size()));
        if (inserted)
        {
            c.blobBuffer.insert(c.blobBuffer.end(), bytes, bytes + kPageBytes);
            char line[48];
            std::snprintf(line, sizeof line, "%u %016llx\n", found->second, (unsigned long long)hash);
            c.blobIndex += line;
            if (c.blobBuffer.size() >= (4u << 20))
            {
                c.writer.Push(kBlobFile, std::move(c.blobBuffer));
                c.blobBuffer.clear();
                c.writer.Push(kBlobIndexFile, c.blobIndex);
                c.blobIndex.clear();
            }
        }
        return found->second;
    }

    void Version(uint32_t page, const uint8_t* bytes, uint64_t hash, bool own)
    {
        Line("%u %05x %u%s\n", c.packet, page, Blob(bytes, hash), own ? " own" : "");
        c.pageLines++;
        c.totalVersions++;
        c.totalOwn += own;
    }

    void Event(const char* text)
    {
        c.lines += text;
        c.eventLines++;
        c.totalEvents++;
    }

    bool InRing(uint32_t page) { return page >= c.ringFirst && page <= c.ringLast; }

    // How often the watch has seen the title write a page of the bank, and whether
    // that is more often than the capture has accounted for.
    uint32_t Loosened(uint32_t page)
    {
        return page - kBankFirst < kBankPages ? gpu::watch::Loosened(page - kBankFirst) : 0;
    }

    // A page the GPU side reads: stored when the replay would hold anything else
    // there, or when the title wrote it since, even with the bytes it held -- the
    // replay's write is then seen by the watch as the title's was.
    void Examine(uint32_t page)
    {
        const uint32_t stamp = c.batch + 1;
        const bool bank = page - kBankFirst < kBankPages;
        const uint32_t loosened = bank ? Loosened(page) : 0;
        const bool written = bank && loosened != c.seenLoose[page - kBankFirst];
        if (c.examined[page] == stamp && !(c.flags[page] & kDirty) && !written) return;
        if (InRing(page)) return;
        c.examined[page] = stamp;
        if (bank) c.seenLoose[page - kBankFirst] = loosened;
        alignas(64) uint8_t copy[kPageBytes];
        std::memcpy(copy, guest::Base() + uint64_t(page) * kPageBytes, kPageBytes);
        const uint64_t hash = PageHash(copy);
        const uint8_t flags = c.flags[page];
        c.flags[page] = 0;
        if (hash == c.held[page] && !written)
        {
            if (flags & kOwnPending) Version(page, copy, hash, true);
            return;
        }
        c.touched += hash == c.held[page];
        c.held[page] = hash;
        Version(page, copy, hash, false);
    }

    void Start()
    {
        if (!c.writer.Open(c.dir)) { c.wanted = false; return; }
        c.held.assign(kPages, 0);
        c.examined.assign(kPages, 0);
        c.flags.assign(kPages, 0);
        c.seenLoose.assign(kBankPages, 0);
        static const uint8_t zero[kPageBytes] = {};
        c.zeroHash = PageHash(zero);
        std::fill(c.held.begin(), c.held.end(), c.zeroHash);
        for (uint32_t p = 0; p < kBankPages; p++) c.seenLoose[p] = gpu::watch::Loosened(p);

        uint32_t ringBase = 0, ringDwords = 0, writeBack = 0;
        RingLayout(ringBase, ringDwords, writeBack);
        c.ringFirst = ringBase >> 12;
        c.ringLast = (ringBase + ringDwords * 4 - 1) >> 12;
        const uint32_t top = kernel::PhysicalHighWater();

        std::string header;
        char line[160];
        std::snprintf(line, sizeof line, "mw2 gpu capture %u\nafter %llu presents %llu\n", kFormatVersion,
                      (unsigned long long)c.after, (unsigned long long)c.presents);
        header += line;
        std::snprintf(line, sizeof line, "ring %08x %u\nwriteback %08x\ntop %08x\n",
                      kernel::ToPhysical(ringBase), ringDwords, writeBack ? kernel::ToPhysical(writeBack) : 0, top);
        header += line;

        // Every page of physical memory in use that is not zero, and the aperture's.
        std::string pages;
        alignas(64) uint8_t copy[kPageBytes];
        const auto snapshot = [&](uint32_t page) {
            std::memcpy(copy, guest::Base() + uint64_t(page) * kPageBytes, kPageBytes);
            const uint64_t hash = PageHash(copy);
            if (page >= kApertureFirst && page <= kApertureLast)
                c.apertureAtStart.insert(c.apertureAtStart.end(), copy, copy + kPageBytes);
            if (hash == c.zeroHash) return;
            c.held[page] = hash;
            char entry[32];
            std::snprintf(entry, sizeof entry, "%05x %u\n", page, Blob(copy, hash));
            pages += entry;
            c.snapshotPages++;
        };
        for (uint32_t page = kBankFirst; page < (top >> 12); page++) snapshot(page);
        for (uint32_t page = kApertureFirst; page <= kApertureLast; page++) snapshot(page);
        std::snprintf(line, sizeof line, "snapshot %llu\n", (unsigned long long)c.snapshotPages);
        header += line;
        header += pages;
        c.writer.Push(kIndexFile, header);

        std::vector<uint8_t> state;
        SaveCommandProcessor(state);
        c.writer.Push(kStateFile, std::move(state));
        c.arenaBlock = SaveArena().block;

        c.started = true;
        capturing = counting = true;
        c.batch = c.packet = 0;
        LOGI("capture: started at swap packet %llu: %llu pages in the snapshot, physical memory to %08X",
             (unsigned long long)SwapsParsed(), (unsigned long long)c.snapshotPages, top);
    }

    void Finish(const char* why)
    {
        char line[96];
        std::snprintf(line, sizeof line, "end %u\n", c.batch);
        c.writer.Push(kIndexFile, line);
        c.writer.Push(kBlobFile, std::move(c.blobBuffer));
        c.blobBuffer.clear();
        c.writer.Push(kBlobIndexFile, c.blobIndex);
        c.blobIndex.clear();
        c.writer.Close();
        capturing = counting = false;
        c.wanted = false;
        LOGI("capture: %s (%s): %u batches, %llu ring dwords, %llu page versions (%llu the consumer's own,"
             " %llu rewritten unchanged), %zu distinct pages (%llu MB), %llu events: %llu image-pool moves,"
             " %llu waits (%llu timed out), %llu aperture reads, %llu arena blocks%s",
             c.dir.c_str(), why, c.batch, (unsigned long long)c.totalRing, (unsigned long long)c.totalVersions,
             (unsigned long long)c.totalOwn, (unsigned long long)c.touched, c.blobs.size(),
             (unsigned long long)(c.blobs.size() * kPageBytes >> 20), (unsigned long long)c.totalEvents,
             (unsigned long long)c.pool, (unsigned long long)c.waits, (unsigned long long)c.waitTimeouts,
             (unsigned long long)c.apertureReads, (unsigned long long)c.arenaBlocks,
             c.writer.Failed() ? "; WRITING FAILED" : "");
        c.held = {};
        c.examined = {};
        c.flags = {};
        c.seenLoose = {};
        c.blobs = {};
    }

    // ---- the replay

    struct Replay
    {
        std::atomic<const Mw2ReplayBatch*> handed{ nullptr };
        const Mw2ReplayBatch* batch = nullptr;
        uint64_t page = 0, event = 0;
        uint32_t packet = 0;
        uint64_t batches = 0;
        // The packet's `own` pages, in journal order. The capture writes one at the first
        // read of a page after the consumer wrote it (in this packet or before), so the
        // replay checks one at the first read of its page since the replay's consumer
        // wrote it (`wrote`, as the capture's kOwnPending), or once the packet is done. A
        // read before the write (an image-pool move whose source and destination share
        // a page) is not that read.
        struct Own
        {
            uint32_t page;
            const uint8_t* bytes;
        };
        std::vector<Own> own;
        std::vector<uint8_t> wrote;   // per page: the replay's consumer wrote it since its last read
        std::deque<Mw2ReplayEvent> waits;
        uint64_t divergences = 0, written = 0, verified = 0, unread = 0, queued = 0, waitEvents = 0, timedOut = 0;
        uint64_t apertureWrites = 0, arenaBlocks = 0;
        uint64_t waitTimeouts = 0;   // the driver's, at the end
        bool finished = false;
    };
    Replay r;

    void Verify(uint32_t page, const uint8_t* bytes);

    // The packet is done: its `own` pages not read are checked as it left them.
    void VerifyUnread()
    {
        for (const auto& o : r.own)
        {
            r.unread++;
            Verify(o.page, o.bytes);
        }
        r.own.clear();
    }

    void Verify(uint32_t page, const uint8_t* bytes)
    {
        uint8_t* at = guest::Base() + uint64_t(page) * kPageBytes;
        if (PageHash(at) == PageHash(bytes)) { r.verified++; return; }
        char what[112];
        std::snprintf(what, sizeof what, "page %05x, which the consumer wrote, holds other bytes than in the capture",
                      page);
        gpu::stream::Diverged(what);
        std::memcpy(at, bytes, kPageBytes);
    }

    // The journal's entries for packet `packet`: pages written, pages to verify,
    // events made. Packet 0's pages the driver writes itself, before the ring.
    void Apply(uint32_t packet)
    {
        // Not read again in the packet the capture read them in (the replay's watch
        // and texture cache began with nothing): checked as the packet left them.
        VerifyUnread();
        for (size_t i = 0; i < r.waits.size(); i++) gpu::stream::Diverged("a WAIT_REG_MEM the replay did not meet");
        r.waits.clear();
        if (!r.batch) return;
        while (r.page < r.batch->pageCount && r.batch->pages[r.page].packet <= packet)
        {
            const Mw2ReplayPage& entry = r.batch->pages[r.page++];
            if (entry.own)
            {
                if (packet == 0) Verify(entry.page, entry.bytes);
                else r.own.push_back({ entry.page, entry.bytes });
                continue;
            }
            if (entry.packet == 0) continue;
            // A plain store: the watch takes the fault the title's write took.
            std::memcpy(guest::Base() + uint64_t(entry.page) * kPageBytes, entry.bytes, kPageBytes);
            r.written++;
        }
        while (r.event < r.batch->eventCount && r.batch->events[r.event].packet <= packet)
        {
            const Mw2ReplayEvent& e = r.batch->events[r.event++];
            switch (e.kind)
            {
            case kMw2ReplayPool:
                imagepool::QueueForReplay(e.a, e.b, e.c, e.d, e.e);
                r.queued++;
                break;
            case kMw2ReplayWait:
                r.waits.push_back(e);
                break;
            case kMw2ReplayAperture:
                *GuestPtr<be32>(kRegisterAperture + e.a * 4) = e.b;
                r.apertureWrites++;
                break;
            case kMw2ReplayArena:
                SetArenaBlock(e.a);
                r.arenaBlocks++;
                break;
            default:
                gpu::stream::Diverged("an event of a kind this build does not know");
            }
        }
    }
}

void gpu::stream::PacketSlow()
{
    if (replaying)
    {
        r.packet++;
        Apply(r.packet);
        return;
    }
    c.packet++;
}

void gpu::stream::ReadPhysicalSlow(uint32_t physical, uint64_t bytes)
{
    if (const uint32_t va = kernel::FromPhysical(physical)) ReadSlow(va, bytes);
}

void gpu::stream::ReadSlow(uint32_t address, uint64_t bytes)
{
    address = Normalised(address);
    const uint32_t first = address >> 12;
    const uint32_t last = uint32_t(std::min<uint64_t>(uint64_t(address) + bytes - 1, 0xFFFFFFFFull) >> 12);
    if (replaying)
    {
        if (r.wrote.empty()) return;
        for (uint32_t page = first; page <= last; page++)
        {
            if (!r.wrote[page]) continue;
            r.wrote[page] = 0;
            for (auto o = r.own.begin(); o != r.own.end(); ++o)
                if (o->page == page)
                {
                    Verify(o->page, o->bytes);
                    r.own.erase(o);
                    break;
                }
        }
        return;
    }
    for (uint32_t page = first; page <= last; page++) Examine(page);
}

void gpu::stream::PolledSlow(uint32_t address)
{
    if (!replaying)
    {
        const uint32_t page = Normalised(address) >> 12;
        if (!InRing(page)) c.flags[page] |= kDirty;
    }
    ReadSlow(address, 4);
}

void gpu::stream::WriteSlow(uint32_t address, uint32_t bytes, bool done)
{
    if (!bytes) return;
    address = Normalised(address);
    const uint32_t first = address >> 12;
    const uint32_t last = uint32_t(std::min<uint64_t>(uint64_t(address) + bytes - 1, 0xFFFFFFFFull) >> 12);
    if (replaying)
    {
        // The replay made the write: the next read of each page checks its `own` entry.
        if (done)
        {
            if (r.wrote.empty()) r.wrote.assign(kPages, 0);
            for (uint32_t page = first; page <= last; page++) r.wrote[page] = 1;
        }
        return;
    }
    if (!done) c.writable.assign(last - first + 1, 0);
    for (uint32_t page = first; page <= last; page++)
    {
        if (InRing(page)) continue;
        const uint8_t* at = guest::Base() + uint64_t(page) * kPageBytes;
        const bool bank = page - kBankFirst < kBankPages;
        if (!done)
        {
            // The replay's write lands on what it holds; whether that is what the
            // title's memory held just before is the question.
            const bool written = bank && Loosened(page) != c.seenLoose[page - kBankFirst];
            c.writable[page - first] = !written && PageHash(at) == c.held[page];
            continue;
        }
        if (page - first < c.writable.size() && c.writable[page - first])
        {
            c.held[page] = PageHash(at);
            c.flags[page] = kDirty | kOwnPending;
        }
        else
        {
            c.held[page] = kUnknown;
            c.flags[page] = kDirty;
        }
        // The fault the consumer's own write took is the replay's too.
        if (bank) c.seenLoose[page - kBankFirst] = Loosened(page);
    }
}

void gpu::stream::BatchBegin(uint32_t base, uint32_t readIndex, uint32_t mask, uint32_t dwords)
{
    if (replaying)
    {
        r.batch = r.handed.exchange(nullptr, std::memory_order_acquire);
        if (!r.batch) Diverged("a batch the replay did not hand over");
        r.page = r.event = 0;
        r.packet = 0;
        Apply(0);
        return;
    }
    ReadSwitch();
    if (c.wanted && !c.started && SwapsParsed() >= c.after) Start();
    if (!capturing) return;
    std::vector<uint8_t> ring(size_t(dwords) * 4);
    const uint32_t* words = GuestPtr<uint32_t>(base);
    for (uint32_t i = 0; i < dwords; i++)
        std::memcpy(ring.data() + size_t(i) * 4, &words[(readIndex + i) & mask], 4);
    c.writer.Push(kRingFile, std::move(ring));
    c.ringDwords = dwords;
    c.totalRing += dwords;
    c.packet = 0;
}

void gpu::stream::BatchEnd()
{
    if (replaying)
    {
        VerifyUnread();
        for (size_t i = 0; i < r.waits.size(); i++) Diverged("a WAIT_REG_MEM the replay did not meet");
        r.waits.clear();
        // Entries for packets the replay never reached.
        if (r.batch)
            for (uint64_t i = r.page + r.event; i < r.batch->pageCount + r.batch->eventCount; i++)
                Diverged("an entry of the journal past the batch's last packet");
        r.batch = nullptr;
        r.batches++;
        return;
    }
    if (!capturing) return;
    char header[96];
    std::snprintf(header, sizeof header, "batch %u ring %u pages %u events %u\n", c.batch, c.ringDwords,
                  c.pageLines, c.eventLines);
    c.writer.Push(kIndexFile, header + c.lines);
    c.lines.clear();
    c.pageLines = c.eventLines = 0;
    c.batch++;
    c.packet = 0;
    if (SwapsParsed() >= c.after + c.presents) Finish("done");
}

void gpu::stream::Shutdown()
{
    if (capturing) Finish("the run ended first");
}

void gpu::stream::ApertureRead(uint32_t index, uint32_t value)
{
    uint32_t held = 0;
    if (const auto found = c.aperture.find(index); found != c.aperture.end()) held = found->second;
    else
    {
        const uint32_t offset = kRegisterAperture + index * 4 - (kApertureFirst << 12);
        if (offset + 4 <= c.apertureAtStart.size())
        {
            std::memcpy(&held, c.apertureAtStart.data() + offset, 4);
            held = __builtin_bswap32(held);
        }
    }
    if (held == value) return;
    c.aperture[index] = value;
    char text[64];
    std::snprintf(text, sizeof text, "aperture %u %x %08x\n", c.packet, index, value);
    Event(text);
    c.apertureReads++;
}

void gpu::stream::ArenaBlockSeen(uint32_t block)
{
    if (block == c.arenaBlock) return;
    c.arenaBlock = block;
    char text[48];
    std::snprintf(text, sizeof text, "arena %u %08x\n", c.packet, block);
    Event(text);
    c.arenaBlocks++;
}

void gpu::stream::WaitDone(bool memory, uint32_t address, uint32_t value, uint32_t polls, bool timedOut)
{
    char text[96];
    std::snprintf(text, sizeof text, "wait %u %u %08x %08x %u %u\n", c.packet, memory ? 1u : 0u, address, value,
                  polls, timedOut ? 1u : 0u);
    Event(text);
    c.waits++;
    c.waitTimeouts += timedOut;
    // The title's write that satisfied it: the replay writes that dword alone.
    if (memory && polls > 1)
        if (const uint32_t va = kernel::FromPhysical(address & ~3u))
        {
            const uint32_t page = Normalised(va) >> 12;
            c.held[page] = kUnknown;
            c.flags[page] = kDirty;
        }
}

bool gpu::stream::ReplayWait(Wait& wait)
{
    if (r.waits.empty()) return false;
    const Mw2ReplayEvent e = r.waits.front();
    r.waits.pop_front();
    wait.memory = e.a != 0;
    wait.value = e.c;
    wait.polls = std::max<uint32_t>(e.d, 1);
    wait.timedOut = e.e != 0;
    r.waitEvents++;
    r.timedOut += wait.timedOut;
    return true;
}

void gpu::stream::PoolSeen(uint32_t destination, uint32_t source, uint32_t size, uint32_t streamConstant,
                           uint64_t frame)
{
    char text[112];
    std::snprintf(text, sizeof text, "pool %u %08x %08x %x %08x %llu\n", c.packet, destination, source, size,
                  streamConstant, (unsigned long long)frame);
    Event(text);
    c.pool++;
}

void gpu::stream::Diverged(const char* what)
{
    if (r.divergences++ < 16)
        LOGW("replay: divergence at batch %llu packet %u: %s", (unsigned long long)r.batches, r.packet, what);
}

void gpu::stream::Report()
{
    if (!replaying) return;
    LOGI("replay: %llu divergences, %llu wait timeouts", (unsigned long long)r.divergences,
         (unsigned long long)r.waitTimeouts);
    LOGI("replay: %llu batches, %llu pages written, %llu pages the consumer wrote found as captured"
         " (%llu of them checked after the packet that read them in the capture), %llu image-pool moves queued, %llu waits (%llu timed out as in the capture), %llu aperture"
         " registers, %llu arena blocks%s",
         (unsigned long long)r.batches, (unsigned long long)r.written, (unsigned long long)r.verified,
         (unsigned long long)r.unread, (unsigned long long)r.queued, (unsigned long long)r.waitEvents, (unsigned long long)r.timedOut,
         (unsigned long long)r.apertureWrites, (unsigned long long)r.arenaBlocks,
         r.finished ? "" : "; the replay did not finish");
}

// The replay driver's calls (src/runtime/gpu_replay.rs).
extern "C"
{
    void mw2_replay_begin()
    {
        replaying = counting = true;
        LOGI("replay: the command processor takes its input from the capture");
    }

    // Before the batch's ring dwords and write pointer, with the consumer caught up.
    void mw2_replay_batch(const Mw2ReplayBatch* batch) { r.handed.store(batch, std::memory_order_release); }

    uint32_t mw2_replay_consumer_read()
    {
        uint32_t read = 0, written = 0, writeBack = 0;
        RingPosition(read, written, writeBack);
        return read;
    }

    void mw2_replay_finished(uint64_t waitTimeouts)
    {
        r.waitTimeouts = waitTimeouts;
        r.finished = true;
    }

    // The hash the capture names its pages by, for the driver's checks.
    uint64_t mw2_replay_page_hash(const uint8_t* bytes) { return PageHash(bytes); }
}

#endif
