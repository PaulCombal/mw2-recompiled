// tools/xma_diff.py's program: the XMA decoder's C++ (runtime/apu/xma.cpp) and its Rust
// (src/runtime/kernel/xma.rs) replaying one recording (MW2_XMA_RECORD, see xma.rs) in one
// process, each on a guest space of its own: mw2_guest_base is switched to a side's before
// anything of it runs.
//
// The recording holds what the title did to the contexts in a real run: which it was
// given and gave back, and for every clear and kick the context block as the title had
// left it and the input buffers' bytes. Before each, both sides get that block and those
// buffers at the recorded addresses; after it, the context block, the whole output ring
// and the log must be the same on both. The decoder's own state between kicks -- the frame
// in flight, FFmpeg's overlap -- is each side's, so a difference there shows in a later
// ring.
//
// A fuzz seed changes some kicks first, the same way on both sides: fields of the block
// (offsets, loops, counts, flags, a pointer), bits of the input, and stores to registers
// the title never writes, and at the end runs the paths the title never takes -- every
// context in use, a release of what is no context.
//
// Both sides share one FFmpeg. Its log goes to the shim's callback (runtime/apu/
// xma_codec.cpp, set last), and mw2_xma_count_error here counts it to the running side.
#include <signal.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/wait.h>
#include <unistd.h>
#include <algorithm>
#include <cstdarg>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <unordered_map>
#include <vector>

extern "C"
{
    extern uint8_t* mw2_guest_base;
    void rig_set_contexts_at(uint32_t address);

    void rig_rust_initialise();
    uint32_t rig_rust_allocate();
    void rig_rust_release(uint32_t address);
    void rig_rust_register_store(uint32_t address, uint32_t value);
    void rig_rust_report();
    bool rig_rust_count_error();
    bool rig_rust_shadow();

    // runtime/apu/xma.cpp, its names with cpp_ in front.
    void cpp_mw2_xma_initialise();
    uint32_t cpp_mw2_xma_allocate_context();
    void cpp_mw2_xma_release_context(uint32_t address);
    void cpp_mw2_xma_register_store(uint32_t address, uint32_t value);
    void cpp_mw2_xma_report();
    extern uint32_t cpp_xma_errors;   // its std::atomic<uint32_t> g_errors
}

namespace
{
    constexpr uint64_t kSpace = 1ull << 32;
    constexpr uint32_t kWindow = 0x7FEA0000u;
    constexpr uint32_t kRegKick0 = 0x650, kRegLock0 = 0x690, kRegClear0 = 0x6A0;
    constexpr uint32_t kContextSize = 64;

    struct Random
    {
        uint64_t state;
        uint64_t Next()
        {
            uint64_t z = (state += 0x9E3779B97F4A7C15ull);
            z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ull;
            z = (z ^ (z >> 27)) * 0x94D049BB133111EBull;
            return z ^ (z >> 31);
        }
        uint32_t Below(uint32_t n) { return uint32_t(Next() % n); }
        bool Chance(uint32_t percent) { return Below(100) < percent; }
    };

    struct Side
    {
        const char* name;
        uint8_t* base;
        std::string log;
        bool rust;
    };
    Side g_sides[2] = { { "C++", nullptr, {}, false }, { "Rust", nullptr, {}, true } };
    Side* g_running = &g_sides[0];

    void Append(const char* format, va_list arguments)
    {
        char line[2048];
        std::vsnprintf(line, sizeof line, format, arguments);
        std::string text = line;
        while (!text.empty() && text.back() == '\n') text.pop_back();
        g_running->log += text + "\n";
    }

    void Enter(Side& side)
    {
        g_running = &side;
        mw2_guest_base = side.base;
    }

    uint32_t Big32(const uint8_t* p) { return uint32_t(p[0]) << 24 | p[1] << 16 | p[2] << 8 | p[3]; }

    // The guest address of a physical one, as both sides find it (runtime/kernel/physical.h
    // with the rig's heap top).
    uint32_t FromPhysical(uint32_t physical)
    {
        if (physical < 0x40000000u) return 0xA0000000u + physical;
        uint32_t cached = 0x80000000u + physical;
        return (cached >= 0x82000000u && cached < 0xA0000000u) ? cached : 0;
    }

    // Each side's call.
    void Initialise(Side& s) { Enter(s); s.rust ? rig_rust_initialise() : cpp_mw2_xma_initialise(); }
    uint32_t Allocate(Side& s) { Enter(s); return s.rust ? rig_rust_allocate() : cpp_mw2_xma_allocate_context(); }
    void Release(Side& s, uint32_t a) { Enter(s); s.rust ? rig_rust_release(a) : cpp_mw2_xma_release_context(a); }
    void Report(Side& s) { Enter(s); s.rust ? rig_rust_report() : cpp_mw2_xma_report(); }
    // `value` as the hardware holds it; the C++ takes what stw would write.
    void Store(Side& s, uint32_t address, uint32_t value)
    {
        Enter(s);
        if (s.rust) rig_rust_register_store(address, value);
        else cpp_mw2_xma_register_store(address, __builtin_bswap32(value));
    }
    void StoreRegister(uint32_t index, uint32_t value)
    {
        for (Side& s : g_sides) Store(s, kWindow + index * 4, value);
    }

    struct Replay
    {
        std::vector<uint8_t> data;
        size_t at = 0;
        uint32_t base = 0;
        std::unordered_map<uint64_t, std::vector<uint8_t>> blobs;
        Random random{ 0 };
        bool fuzz = false;
        uint32_t step = 0;
        uint32_t kicks = 0, clears = 0, fuzzed = 0, hangs = 0, faults = 0;

        template <typename T> T Take()
        {
            if (at + sizeof(T) > data.size()) { std::fprintf(stdout, "the recording ends inside a record\n"); std::exit(2); }
            T value; std::memcpy(&value, &data[at], sizeof value); at += sizeof value;
            return value;
        }
        const uint8_t* TakeBytes(size_t size)
        {
            if (at + size > data.size()) { std::fprintf(stdout, "the recording ends inside a record\n"); std::exit(2); }
            at += size;
            return &data[at - size];
        }

        [[noreturn]] void Different(const char* what)
        {
            std::printf("DIFFERENT at step %u (%u kicks in): %s\n", step, kicks, what);
            for (Side& s : g_sides)
                std::printf("--- %s's log:\n%s", s.name, s.log.c_str());
            std::exit(1);
        }

        void CompareLogs()
        {
            if (g_sides[0].log != g_sides[1].log) Different("the logs");
            g_sides[0].log.clear();
            g_sides[1].log.clear();
        }

        void CompareMemory(uint32_t address, uint32_t size, const char* what)
        {
            if (!size) return;
            const uint8_t* a = g_sides[0].base + address;
            const uint8_t* b = g_sides[1].base + address;
            if (std::memcmp(a, b, size) == 0) return;
            uint32_t first = 0;
            while (a[first] == b[first]) first++;
            char text[256];
            std::snprintf(text, sizeof text, "%s at %08X, first at +%u: C++ %02X, Rust %02X", what, address, first,
                          a[first], b[first]);
            if (size == kContextSize)
            {
                std::string words = text;
                for (uint32_t i = 0; i < 16; i++)
                    if (Big32(a + i * 4) != Big32(b + i * 4))
                    {
                        char word[64];
                        std::snprintf(word, sizeof word, "\n  dword %u: C++ %08X Rust %08X", i, Big32(a + i * 4), Big32(b + i * 4));
                        words += word;
                    }
                Different(words.c_str());
            }
            Different(text);
        }

        void Write(uint32_t address, const uint8_t* bytes, size_t size)
        {
            for (Side& s : g_sides) std::memcpy(s.base + address, bytes, size);
        }

        uint32_t ContextAt(uint16_t id) { return base + id * kContextSize; }

        // The fields as xma.rs names them: word, shift, width.
        void SetField(uint8_t* block, int word, int shift, int width, uint32_t value)
        {
            uint32_t v = Big32(block + word * 4);
            const uint32_t mask = (width == 32 ? ~0u : ((1u << width) - 1)) << shift;
            v = (v & ~mask) | ((value << shift) & mask);
            const uint32_t big = __builtin_bswap32(v);
            std::memcpy(block + word * 4, &big, 4);
        }

        bool drainNext = false;
        uint16_t drainId = 0;

        void FuzzBlock(uint8_t* block, uint32_t packets)
        {
            switch (random.Below(18))
            {
            case 17:   // both buffers valid, the second the same as the first
            {
                const uint32_t current = Big32(block + 4 * 4) >> 31;
                const uint32_t pointer = Big32(block + (5 + current) * 4);
                const uint32_t count = current ? Big32(block + 4) & 0xFFF : Big32(block) & 0xFFF;
                SetField(block, 5 + (current ^ 1), 0, 32, pointer);
                SetField(block, current ? 0 : 1, 0, 12, count);
                SetField(block, 0, 20, 1, 1);
                SetField(block, 0, 21, 1, 1);
                break;
            }
            case 15:   // the read offset at the end of the buffer, or just before
                SetField(block, 2, 0, 26, packets * 16384 - random.Below(2));
                break;
            case 16:   // a loop that wraps on this kick, skipping into the frame after
            {
                const uint32_t offset = Big32(block + 2 * 4) & 0x3FFFFFF;
                SetField(block, 4, 0, 26, offset);
                SetField(block, 0, 12, 8, random.Chance(50) ? 255 : 1 + random.Below(3));
                SetField(block, 1, 17, 3, random.Chance(50) ? 4 : random.Below(8));
                break;
            }
            case 14:
                // A ring of two blocks, read from the start, so a frame is left in flight;
                // the context's next kick has no input, only that frame to drain.
                SetField(block, 0, 22, 5, 2);
                SetField(block, 9, 0, 5, 0);
                SetField(block, 0, 27, 5, 0);
                SetField(block, 1, 20, 4, 1);
                SetField(block, 1, 24, 3, 0);
                SetField(block, 1, 31, 1, 1);
                drainNext = true;
                break;
            case 0: SetField(block, 2, 0, 26, random.Below(packets * 2048 * 8 + 64)); break;   // read offset
            case 1: SetField(block, 0, 12, 8, random.Chance(30) ? 255 : random.Below(4)); break;   // loop count
            case 2: SetField(block, 3, 0, 26, random.Below(packets * 2048 * 8 + 64)); break;   // loop start
            case 3: SetField(block, 4, 0, 26, random.Below(packets * 2048 * 8 + 64)); break;   // loop end
            case 4: SetField(block, 1, 20, 4, random.Below(16)); break;   // subframe decode count
            case 5: SetField(block, 1, 24, 3, random.Below(8)); break;    // output padding
            // Output block count; 0 with the ring valid has the C++ divide by it, so only the
            // Rust runs it.
            case 6: SetField(block, 0, 22, 5, random.Below(32)); break;
            case 7: SetField(block, 4, 31, 1, random.Below(2)); break;    // current buffer
            case 8: SetField(block, 0, 20 + random.Below(2), 1, random.Below(2)); break;   // input valid
            // Not stereo: a context turned mono with a stereo frame in flight has the C++
            // copy from before the frame (the Rust writes silence), and the title never
            // changes a live context's channels.
            case 9: SetField(block, 1, 27, 2, random.Below(4)); break;    // sample rate
            case 10: SetField(block, 1, 27, 2, random.Below(4)); break;   // sample rate
            case 11: SetField(block, 5 + random.Below(2), 0, 32, 0); break;   // an input pointer gone
            case 12: SetField(block, 1, 14, 6, random.Below(64)); break;  // loop subframe end and skip
            default:
                SetField(block, 9, 0, 5, random.Below(32));                 // read and write offsets
                SetField(block, 0, 27, 5, random.Below(32));
                break;
            }
        }

        // `width` bits at bit `at`, big-endian.
        static void PutBits(std::vector<uint8_t>& bytes, uint32_t at, uint32_t width, uint32_t value)
        {
            for (uint32_t i = 0; i < width && (at + i) / 8 < bytes.size(); i++)
            {
                const uint32_t bit = at + i;
                const uint8_t mask = uint8_t(0x80u >> (bit % 8));
                if ((value >> (width - 1 - i)) & 1) bytes[bit / 8] |= mask;
                else bytes[bit / 8] &= uint8_t(~mask);
            }
        }

        // `offset`: the bit the context will read next.
        void FuzzInput(std::vector<uint8_t>& bytes, uint32_t offset)
        {
            const uint32_t packets = uint32_t(bytes.size() / 2048);
            if (!packets) return;
            for (uint32_t n = 1 + random.Below(3); n; n--)
            {
                const uint32_t packet = random.Below(packets) * 2048;
                // The packet holding the frame read next, when the offset is in this buffer.
                const uint32_t here = offset / 8 < bytes.size() ? offset / 16384 * 2048 : packet;
                if (random.Chance(40) && offset / 8 + 16 < bytes.size())
                    bytes[offset / 8 + random.Below(16)] ^= uint8_t(1u << random.Below(8));   // the frame read next
                else if (random.Chance(15) && offset / 8 + 4 < bytes.size())
                    PutBits(bytes, offset, 15, random.Chance(50) ? 0x7FFF : 1 + random.Below(7));   // "none", or shorter than a byte
                else if (random.Chance(15))
                {
                    // A first frame offset at its limits, from "none starts here" down to the
                    // last that does, with a skip to the next packet of the stream.
                    // On this packet, one after it, or the buffer's first, which is where the
                    // other buffer starts.
                    static const uint32_t kOffsets[] = { 0x7FFF, 16321, 16320, 16319 };
                    uint32_t target = here;
                    if (random.Chance(40)) target = std::min<uint32_t>(here + 2048 * (1 + random.Below(3)), (packets - 1) * 2048);
                    else if (random.Chance(40)) target = 0;
                    PutBits(bytes, target * 8 + 6, 15, kOffsets[random.Below(4)]);
                    if (random.Chance(70)) bytes[target + 3] = uint8_t(random.Chance(30) ? 0xFF : random.Below(3));
                }
                else if (random.Chance(15))
                {
                    // An XMA2 header's frame count, which may name more frames than the walk
                    // finds.
                    bytes[here] = uint8_t((bytes[here] & 3) | (random.Below(64) << 2));
                    bytes[here + 2] = uint8_t((bytes[here + 2] & ~7) | 1);
                }
                else if (random.Chance(50))
                    bytes[packet + random.Below(4)] ^= uint8_t(1u << random.Below(8));   // the header
                else if (random.Chance(10))
                    bytes[packet + 3] = 0xFF;                                         // no frame starts here
                else
                    bytes[packet + 4 + random.Below(2044)] ^= uint8_t(1u << random.Below(8));
            }
        }

        // Whether `side` would never return from these stores, or fault: tried in a child,
        // which is given two seconds.
        enum class Trial { kReturns, kHangs, kFaults };
        Trial Try(Side& side, const std::vector<std::pair<uint32_t, uint32_t>>& stores)
        {
            std::fflush(stdout);
            const pid_t child = fork();
            if (child == 0)
            {
                for (const auto& [index, value] : stores) Store(side, kWindow + index * 4, value);
                _exit(0);
            }
            for (int waited = 0; waited < 2000; waited++)
            {
                int status = 0;
                if (waitpid(child, &status, WNOHANG) == child)
                    return WIFEXITED(status) && WEXITSTATUS(status) == 0 ? Trial::kReturns : Trial::kFaults;
                usleep(1000);
            }
            kill(child, SIGKILL);
            waitpid(child, nullptr, 0);
            return Trial::kHangs;
        }

        void Kick()
        {
            const uint16_t id = Take<uint16_t>();
            uint8_t recorded[kContextSize];
            std::memcpy(recorded, TakeBytes(kContextSize), kContextSize);
            const uint8_t count = Take<uint8_t>();
            std::vector<std::pair<uint32_t, const std::vector<uint8_t>*>> buffers;
            uint32_t largest = 1;
            for (uint8_t i = 0; i < count; i++)
            {
                const uint32_t physical = Take<uint32_t>();
                const uint64_t key = Take<uint64_t>();
                Take<uint32_t>();   // the size, which the bytes have
                auto found = blobs.find(key);
                if (found == blobs.end()) { std::printf("the recording names a buffer it never gave\n"); std::exit(2); }
                buffers.emplace_back(FromPhysical(physical), &found->second);
                largest = std::max<uint32_t>(largest, uint32_t(found->second.size() / 2048));
            }
            const uint32_t context = ContextAt(id);
            const std::vector<std::pair<uint32_t, uint32_t>> kick = {
                { kRegLock0 + id / 32, 1u << (id % 32) }, { kRegKick0 + id / 32, 1u << (id % 32) } };

            bool changed = false;
            const bool drain = drainNext && drainId == id;
            if (drain || (fuzz && random.Chance(8)))
            {
                // What the title would never give: tried on the C++ alone first, as the
                // decoder, Xenia's included, loops for ever on some damaged streams --
                // a loop of frames that all fail, a packet header that sends the offset
                // back over a frame that fails -- and the C++ copies out of bounds on
                // others, where the Rust stops at the buffer's end; neither compares, so
                // there the Rust only has to return or hang as the C++ does.
                uint8_t block[kContextSize];
                std::memcpy(block, recorded, kContextSize);
                std::vector<std::vector<uint8_t>> inputs;
                for (const auto& [address, bytes] : buffers)
                {
                    inputs.push_back(*bytes);
                    if (random.Chance(40)) FuzzInput(inputs.back(), Big32(recorded + 2 * 4) & 0x3FFFFFF);
                }
                drainNext = false;
                if (drain)
                {
                    SetField(block, 0, 0, 12, 0);
                    SetField(block, 1, 0, 12, 0);
                    // Now and then room for one block only, which empties the ring.
                    if (random.Chance(50))
                    {
                        SetField(block, 0, 22, 5, 2);
                        SetField(block, 9, 0, 5, 1);
                        SetField(block, 0, 27, 5, 0);
                        SetField(block, 1, 20, 4, 1);
                        SetField(block, 1, 31, 1, 1);
                    }
                }
                else
                    for (uint32_t n = 1 + random.Below(2); n; n--) FuzzBlock(block, largest);
                if (drainNext) drainId = id;
                std::vector<std::pair<uint32_t, uint32_t>> stores;
                if (random.Chance(5)) stores.emplace_back(0x601, random.Below(4));
                if (random.Chance(3)) stores.emplace_back(0x400 + random.Below(0x100), uint32_t(random.Next()));
                if (random.Chance(3)) stores.emplace_back(kRegKick0 + random.Below(10), 1u << random.Below(32));
                stores.insert(stores.end(), kick.begin(), kick.end());
                for (size_t i = 0; i < buffers.size(); i++)
                    Write(buffers[i].first, inputs[i].data(), inputs[i].size());
                Write(context, block, kContextSize);
                const Trial cpp = Try(g_sides[0], stores);
                if (cpp != Trial::kReturns)
                {
                    // Where the C++ fails the Rust may hang as it does, on a loop the
                    // stream never leaves, but must not fault or panic.
                    (cpp == Trial::kHangs ? hangs : faults)++;
                    if (Try(g_sides[1], stores) == Trial::kFaults)
                        Different("the Rust faults or panics on a kick the C++ fails on");
                }
                else
                {
                    changed = true;
                    fuzzed++;
                    for (const auto& [index, value] : stores) StoreRegister(index, value);
                }
            }
            if (!changed)
            {
                for (const auto& [address, bytes] : buffers) Write(address, bytes->data(), bytes->size());
                Write(context, recorded, kContextSize);
                for (const auto& [index, value] : kick) StoreRegister(index, value);
            }
            kicks++;

            CompareMemory(context, kContextSize, "the context block");
            const uint8_t* after = g_sides[0].base + context;
            const uint32_t blocks = (Big32(after) >> 22) & 31;
            const uint32_t output = FromPhysical(Big32(after + 7 * 4));
            CompareMemory(output, blocks * 256, "the output ring");
            CompareLogs();
        }

        void Run()
        {
            // What comes before the decoder is started: nothing.
            for (Side& s : g_sides)
            {
                if (Allocate(s) != 0) Different("an allocation before the start");
                Store(s, kWindow + kRegKick0 * 4, 1);
                Release(s, 0xA0005000u);
            }
            CompareLogs();
            while (at < data.size())
            {
                step++;
                const char kind = char(Take<uint8_t>());
                switch (kind)
                {
                case 'I':
                {
                    base = Take<uint32_t>();
                    rig_set_contexts_at(base);
                    for (Side& s : g_sides) Initialise(s);
                    CompareMemory(kWindow + 0x1800, 0x40, "the registers");
                    CompareMemory(base, 320 * kContextSize, "the contexts");
                    CompareLogs();
                    break;
                }
                case 'B':
                {
                    const uint64_t key = Take<uint64_t>();
                    const uint32_t size = Take<uint32_t>();
                    const uint8_t* bytes = TakeBytes(size);
                    blobs[key].assign(bytes, bytes + size);
                    break;
                }
                case 'A':
                {
                    const uint16_t id = Take<uint16_t>();
                    const uint32_t c = Allocate(g_sides[0]), r = Allocate(g_sides[1]);
                    if (c != r) Different("the context allocated");
                    if (c != ContextAt(id)) std::printf("note: step %u allocated %08X where the run had context %u\n", step, c, id);
                    CompareLogs();
                    break;
                }
                case 'R':
                {
                    const uint16_t id = Take<uint16_t>();
                    for (Side& s : g_sides) Release(s, ContextAt(id));
                    CompareMemory(ContextAt(id), kContextSize, "a released context");
                    CompareLogs();
                    break;
                }
                case 'C':
                {
                    const uint16_t id = Take<uint16_t>();
                    uint8_t block[kContextSize];
                    std::memcpy(block, TakeBytes(kContextSize), kContextSize);
                    if (fuzz && random.Chance(50))
                    {
                        SetField(block, 9, 0, 5, 1 + random.Below(31));   // read and write offsets
                        SetField(block, 0, 27, 5, 1 + random.Below(31));
                        SetField(block, 2, 0, 26, random.Below(1 << 20));
                    }
                    Write(ContextAt(id), block, kContextSize);
                    StoreRegister(kRegClear0 + id / 32, 1u << (id % 32));
                    clears++;
                    CompareMemory(ContextAt(id), kContextSize, "a cleared context");
                    CompareLogs();
                    break;
                }
                case 'K':
                    Kick();
                    break;
                default:
                    std::printf("a record of kind %02X\n", uint8_t(kind));
                    std::exit(2);
                }
            }
            if (fuzz) Exhaust();
            for (Side& s : g_sides) Report(s);
            CompareLogs();
        }

        // What the title never does: ask for more contexts than there are, give back what
        // is no context, store to registers nothing reads.
        void Exhaust()
        {
            std::vector<uint32_t> taken;
            for (;;)
            {
                const uint32_t c = Allocate(g_sides[0]), r = Allocate(g_sides[1]);
                if (c != r) Different("an allocation when nearly all are taken");
                CompareLogs();
                if (!c) break;
                taken.push_back(c);
            }
            for (Side& s : g_sides)
            {
                Release(s, base + 320 * kContextSize);
                Release(s, base - 4);
            }
            for (uint32_t n = 0; n < 12; n++) StoreRegister(0x300 + n, n);
            StoreRegister(kRegLock0 + 9, ~0u);   // contexts 288..319, and bits past the last
            StoreRegister(kRegClear0 + 9, 0x80000001u);
            // Kicked with no ring: counted as used, with no frame decoded.
            for (uint32_t c : taken)
            {
                const uint32_t id = (c - base) / kContextSize;
                StoreRegister(kRegKick0 + id / 32, 1u << (id % 32));
            }
            for (uint32_t c : taken)
                for (Side& s : g_sides) Release(s, c);
            CompareLogs();
        }
    };
}

// The C++'s fprintf(stderr, ...) and src/log.rs's, renamed.
extern "C" int rig_fprintf(FILE*, const char* format, ...)
{
    va_list arguments;
    va_start(arguments, format);
    Append(format, arguments);
    va_end(arguments);
    return 0;
}

extern "C" bool mw2_xma_count_error()
{
    // FFmpeg beside the Rust's port logs nothing: the port logs what it would have.
    if (g_running->rust) return !rig_rust_shadow() && rig_rust_count_error();
    return __atomic_fetch_add(&cpp_xma_errors, 1, __ATOMIC_RELAXED) < 20;
}

int main(int argc, char** argv)
{
    if (argc < 2) { std::printf("usage: xma_diff <recording> [fuzz seed]\n"); return 2; }
    // A replay stuck in a kick that never returns goes with whatever stopped waiting for it.
    prctl(PR_SET_PDEATHSIG, SIGKILL);
    Replay replay;
    FILE* file = std::fopen(argv[1], "rb");
    if (!file) { std::printf("cannot read %s\n", argv[1]); return 2; }
    std::fseek(file, 0, SEEK_END);
    replay.data.resize(size_t(std::ftell(file)));
    std::fseek(file, 0, SEEK_SET);
    if (std::fread(replay.data.data(), 1, replay.data.size(), file) != replay.data.size()) return 2;
    std::fclose(file);
    if (argc > 2)
    {
        replay.fuzz = true;
        replay.random.state = std::strtoull(argv[2], nullptr, 10);
    }
    for (Side& s : g_sides)
    {
        void* space = mmap(nullptr, kSpace, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
        if (space == MAP_FAILED) { std::printf("no guest space\n"); return 2; }
        s.base = static_cast<uint8_t*>(space);
    }
    replay.Run();
    std::printf("all the same: %u kicks (%u changed; %u changes the C++ would hang on and %u it would fault on, run on the Rust alone), %u clears\n",
                replay.kicks, replay.fuzzed, replay.hangs, replay.faults, replay.clears);
    return 0;
}
