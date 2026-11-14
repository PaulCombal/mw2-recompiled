// Asset-free test of the kernel's file layer: how guest NT paths become host
// files. It compiles runtime/kernel/files.cpp unchanged, calls its NtCreateFile,
// NtReadFile and friends the way recompiled code would, and runs them against a
// temporary folder of synthetic files made here. No game files are involved.
#include "tests/fake_guest.h"
#include "runtime/kernel/objects.h"

#ifdef NDEBUG
#undef NDEBUG
#endif
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <string>
#include <unistd.h>

namespace fs = std::filesystem;

PPC_FUNC(__imp__NtCreateFile);
PPC_FUNC(__imp__NtOpenFile);
PPC_FUNC(__imp__NtReadFile);
PPC_FUNC(__imp__NtWriteFile);
PPC_FUNC(__imp__NtQueryInformationFile);
PPC_FUNC(__imp__NtQueryFullAttributesFile);
PPC_FUNC(__imp__NtSetInformationFile);

// ---- what the file sources link against ------------------------------------
static fs::path g_discRoot;
const fs::path& kernel::GameRoot() { return g_discRoot; }
void kernel::QueueUserApc(uint32_t, uint32_t, uint32_t, std::function<void()> work)
{
    if (work) work();   // not delivered later here: run it where it was queued
}

using namespace kernel;

namespace
{
    constexpr uint32_t ACCESS_DENIED = 0xC0000022u;
    constexpr uint32_t GENERIC_READ = 0x80000000u, GENERIC_WRITE = 0x40000000u;
    constexpr uint32_t FILE_SUPERSEDE = 0, FILE_OPEN = 1, FILE_CREATE = 2,
                       FILE_OPEN_IF = 3, FILE_OVERWRITE = 4, FILE_OVERWRITE_IF = 5;
    constexpr uint32_t FILE_OPENED = 1, FILE_CREATED = 2, FILE_OVERWRITTEN = 3;
    constexpr uint32_t FILE_DIRECTORY_FILE = 1;

    // Fixed guest addresses for the structures a call passes by pointer.
    constexpr uint32_t kHandleOut = 0x3000, kAttributes = 0x3010, kAnsi = 0x3020,
                       kName = 0x3030, kIosb = 0x3200, kOffset = 0x3210, kInfo = 0x3300,
                       kBuffer = 0x10000, kStack = 0x8000;

    PPCContext Ctx()
    {
        PPCContext ctx;
        ctx.r1.u64 = kStack;
        return ctx;
    }

    void PutName(const std::string& name)
    {
        std::memcpy(guest::Base() + kName, name.c_str(), name.size() + 1);
        *GuestPtr<be16>(kAnsi) = uint16_t(name.size());
        *GuestPtr<be16>(kAnsi + 2) = uint16_t(name.size() + 1);
        *GuestPtr<be32>(kAnsi + 4) = kName;
        *GuestPtr<be32>(kAttributes) = 0;
        *GuestPtr<be32>(kAttributes + 4) = kAnsi;
        *GuestPtr<be32>(kAttributes + 8) = 0;
    }

    struct Opened { uint32_t status = 0, handle = 0, information = 0; };

    Opened Create(const std::string& name, uint32_t access, uint32_t disposition,
                  uint32_t options = 0)
    {
        PutName(name);
        *GuestPtr<be32>(kHandleOut) = 0;
        PPCContext ctx = Ctx();
        *GuestPtr<be32>(kStack + 0x14 + 8 * 8) = options;   // the ninth argument
        ctx.r3.u64 = kHandleOut;
        ctx.r4.u64 = access;
        ctx.r5.u64 = kAttributes;
        ctx.r6.u64 = kIosb;
        ctx.r10.u64 = disposition;
        __imp__NtCreateFile(ctx, guest::Base());
        return { uint32_t(ctx.r3.u64), *GuestPtr<be32>(kHandleOut), *GuestPtr<be32>(kIosb + 4) };
    }

    uint32_t Open(const std::string& name, uint32_t access = GENERIC_READ)
    {
        PutName(name);
        PPCContext ctx = Ctx();
        ctx.r3.u64 = kHandleOut;
        ctx.r4.u64 = access;
        ctx.r5.u64 = kAttributes;
        ctx.r6.u64 = kIosb;
        __imp__NtOpenFile(ctx, guest::Base());
        return uint32_t(ctx.r3.u64);
    }

    struct Transfer { uint32_t status = 0, information = 0; };

    Transfer Read(uint32_t handle, uint32_t length, int64_t offset = -1)
    {
        PPCContext ctx = Ctx();
        if (offset >= 0) *GuestPtr<be64>(kOffset) = uint64_t(offset);
        ctx.r3.u64 = handle;
        ctx.r7.u64 = kIosb;
        ctx.r8.u64 = kBuffer;
        ctx.r9.u64 = length;
        ctx.r10.u64 = offset >= 0 ? kOffset : 0;
        __imp__NtReadFile(ctx, guest::Base());
        return { uint32_t(ctx.r3.u64), *GuestPtr<be32>(kIosb + 4) };
    }

    Transfer Write(uint32_t handle, const std::string& data, int64_t offset = -1)
    {
        PPCContext ctx = Ctx();
        std::memcpy(guest::Base() + kBuffer, data.data(), data.size());
        if (offset >= 0) *GuestPtr<be64>(kOffset) = uint64_t(offset);
        ctx.r3.u64 = handle;
        ctx.r7.u64 = kIosb;
        ctx.r8.u64 = kBuffer;
        ctx.r9.u64 = data.size();
        ctx.r10.u64 = offset >= 0 ? kOffset : 0;
        __imp__NtWriteFile(ctx, guest::Base());
        return { uint32_t(ctx.r3.u64), *GuestPtr<be32>(kIosb + 4) };
    }

    uint32_t QueryInfo(uint32_t handle, uint32_t infoClass)
    {
        PPCContext ctx = Ctx();
        ctx.r3.u64 = handle;
        ctx.r4.u64 = kIosb;
        ctx.r5.u64 = kInfo;
        ctx.r7.u64 = infoClass;
        __imp__NtQueryInformationFile(ctx, guest::Base());
        return uint32_t(ctx.r3.u64);
    }

    void SetInfo(uint32_t handle, uint32_t infoClass, uint64_t value)
    {
        PPCContext ctx = Ctx();
        *GuestPtr<be64>(kInfo) = value;
        ctx.r3.u64 = handle;
        ctx.r4.u64 = kIosb;
        ctx.r5.u64 = kInfo;
        ctx.r7.u64 = infoClass;
        __imp__NtSetInformationFile(ctx, guest::Base());
    }

    uint32_t FullAttributes(const std::string& name)
    {
        PutName(name);
        PPCContext ctx = Ctx();
        ctx.r3.u64 = kAttributes;
        ctx.r4.u64 = kInfo;
        __imp__NtQueryFullAttributesFile(ctx, guest::Base());
        return uint32_t(ctx.r3.u64);
    }

    std::string Bytes(uint32_t count)
    {
        return std::string(reinterpret_cast<const char*>(guest::Base() + kBuffer), count);
    }

    std::string Slurp(const fs::path& p)
    {
        std::ifstream in(p, std::ios::binary);
        return { std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>() };
    }

    std::string Pattern(size_t n)
    {
        std::string s(n, '\0');
        for (size_t i = 0; i < n; i++) s[i] = char((i * 7 + 3) & 0xFF);
        return s;
    }
}

int main()
{
    // A disc and a save area made of synthetic files, outside the repository.
    const fs::path base = fs::temp_directory_path() / ("mw3-files-test-" + std::to_string(getpid()));
    fs::remove_all(base);
    g_discRoot = base / "disc";
    const fs::path saves = base / "saves";
    fs::create_directories(g_discRoot / "Zone" / "Sub");
    fs::create_directories(saves);
    const std::string data = Pattern(1000);
    { std::ofstream(g_discRoot / "Zone" / "Sub" / "Data.bin", std::ios::binary) << data; }
    { std::ofstream(g_discRoot / "empty.bin", std::ios::binary); }
    { std::ofstream(base / "outside.txt") << "outside the disc"; }

    // ---- paths: case, separators, devices ------------------------------------
    for (const char* name : { "D:\\zone\\sub\\data.bin", "D:\\ZONE\\SUB\\DATA.BIN",
                              "D:\\\\zone\\\\sub\\\\data.bin", "D:\\zone\\.\\sub\\data.bin",
                              "update:\\zone\\sub\\data.bin", "game:\\zone/sub/data.bin",
                              "\\Device\\Cdrom0\\zone\\sub\\data.bin" })
    {
        Opened o = Create(name, GENERIC_READ, FILE_OPEN);
        assert(o.status == X_STATUS_SUCCESS && o.handle && o.information == FILE_OPENED);
        assert(CloseHandle(o.handle));
    }

    // ---- a name that is not there: the requests the title has been seen to make
    for (const char* name : { "D:\\patch.ff", "update:\\patch.ff", "D:\\patch.ffm",
                              "D:\\imagefile4.pak", "D:\\imagefile4.pakm", "D:\\zone\\nothing.bin" })
    {
        Opened o = Create(name, GENERIC_READ, FILE_OPEN);
        assert(o.status == X_STATUS_OBJECT_NAME_NOT_FOUND && o.handle == 0 && o.information == 0);
        assert(Open(name) == X_STATUS_OBJECT_NAME_NOT_FOUND);
    }
    assert(Create("D:\\", GENERIC_READ, FILE_OPEN).status == X_STATUS_OBJECT_NAME_NOT_FOUND);

    // An OBJECT_ATTRIBUTES with no name in it is a different bug, and also not a file.
    PutName("D:\\zone\\sub\\data.bin");
    *GuestPtr<be32>(kAttributes + 4) = 0;
    {
        PPCContext ctx = Ctx();
        ctx.r3.u64 = kHandleOut; ctx.r4.u64 = GENERIC_READ; ctx.r5.u64 = kAttributes; ctx.r6.u64 = kIosb;
        ctx.r10.u64 = FILE_OPEN;
        __imp__NtCreateFile(ctx, guest::Base());
        assert(uint32_t(ctx.r3.u64) == X_STATUS_OBJECT_NAME_NOT_FOUND);
    }

    // ---- no path climbs out of its device -------------------------------------
    for (const char* name : { "D:\\..\\outside.txt", "D:\\zone\\..\\..\\outside.txt",
                              "D:\\..\\..\\..\\outside.txt", "D:/../outside.txt" })
        assert(Create(name, GENERIC_READ, FILE_OPEN).status == X_STATUS_OBJECT_NAME_NOT_FOUND);

    // ---- reading ---------------------------------------------------------------
    {
        Opened o = Create("D:\\zone\\sub\\data.bin", GENERIC_READ, FILE_OPEN);
        Transfer t = Read(o.handle, 16, 100);
        assert(t.status == X_STATUS_SUCCESS && t.information == 16);
        assert(Bytes(16) == data.substr(100, 16));

        t = Read(o.handle, 16);                       // no offset: carries on from there
        assert(t.status == X_STATUS_SUCCESS && Bytes(16) == data.substr(116, 16));

        t = Read(o.handle, 50, 990);                  // a short read at the end
        assert(t.status == X_STATUS_SUCCESS && t.information == 10);
        assert(Bytes(10) == data.substr(990));

        t = Read(o.handle, 16, 1000);                 // at the end: nothing, and EOF
        assert(t.status == X_STATUS_END_OF_FILE && t.information == 0);

        assert(QueryInfo(o.handle, 5) == X_STATUS_SUCCESS);          // FileStandardInformation
        assert(uint64_t(*GuestPtr<be64>(kInfo + 8)) == 1000);
        assert(QueryInfo(o.handle, 34) == X_STATUS_SUCCESS);         // FileNetworkOpenInformation
        assert(uint64_t(*GuestPtr<be64>(kInfo + 32)) == 1000);

        SetInfo(o.handle, 14, 500);                    // FilePositionInformation
        assert(Read(o.handle, 4).status == X_STATUS_SUCCESS && Bytes(4) == data.substr(500, 4));
        assert(QueryInfo(o.handle, 14) == X_STATUS_SUCCESS);
        assert(uint64_t(*GuestPtr<be64>(kInfo)) == 504);
        CloseHandle(o.handle);

        Opened e = Create("D:\\empty.bin", GENERIC_READ, FILE_OPEN);
        assert(Read(e.handle, 8).status == X_STATUS_END_OF_FILE);
        CloseHandle(e.handle);

        assert(Read(0xDEADBEEF, 4).status == X_STATUS_INVALID_PARAMETER);
    }

    // ---- the disc is read-only --------------------------------------------------
    {
        const fs::path onDisc = g_discRoot / "Zone" / "Sub" / "Data.bin";
        Opened o = Create("D:\\zone\\sub\\data.bin", GENERIC_READ | GENERIC_WRITE, FILE_OPEN);
        assert(o.status == X_STATUS_SUCCESS);
        Transfer t = Write(o.handle, "scribble", 0);   // accepted and discarded
        assert(t.status == X_STATUS_SUCCESS && t.information == 8);
        CloseHandle(o.handle);
        assert(Slurp(onDisc) == data);

        o = Create("D:\\zone\\sub\\data.bin", GENERIC_WRITE, FILE_OVERWRITE_IF);   // must not truncate
        assert(o.status == X_STATUS_SUCCESS);
        CloseHandle(o.handle);
        o = Create("D:\\zone\\sub\\data.bin", GENERIC_WRITE, FILE_SUPERSEDE);
        CloseHandle(o.handle);
        assert(Slurp(onDisc) == data);

        assert(Create("D:\\new.bin", GENERIC_WRITE, FILE_OPEN_IF).status == X_STATUS_OBJECT_NAME_NOT_FOUND);
        assert(Create("D:\\new.bin", GENERIC_WRITE, FILE_CREATE).status == X_STATUS_OBJECT_NAME_NOT_FOUND);
        assert(!fs::exists(g_discRoot / "new.bin"));
        assert(Create("D:\\zone\\sub\\data.bin", GENERIC_READ, FILE_CREATE).status == X_STATUS_OBJECT_NAME_COLLISION);
    }

    // ---- directories ------------------------------------------------------------
    {
        Opened d = Create("D:\\zone", GENERIC_READ, FILE_OPEN, FILE_DIRECTORY_FILE);
        assert(d.status == X_STATUS_SUCCESS && d.information == FILE_OPENED);
        assert(QueryInfo(d.handle, 5) == X_STATUS_SUCCESS);
        assert(uint32_t(*GuestPtr<be32>(kInfo + 20)) == 0x01000000u);   // the directory flag
        CloseHandle(d.handle);
        assert(FullAttributes("D:\\zone") == X_STATUS_SUCCESS && uint32_t(*GuestPtr<be32>(kInfo + 48)) == 0x10);
    }

    // ---- full attributes -----------------------------------------------------------
    assert(FullAttributes("D:\\zone\\sub\\DATA.BIN") == X_STATUS_SUCCESS);
    assert(uint64_t(*GuestPtr<be64>(kInfo + 32)) == 1000 && uint64_t(*GuestPtr<be64>(kInfo + 40)) == 1000);
    assert(uint32_t(*GuestPtr<be32>(kInfo + 48)) == 0x80);
    assert(FullAttributes("D:\\patch.ff") == X_STATUS_OBJECT_NAME_NOT_FOUND);

    // ---- Harddisk0\Partition0, a read-only null device -------------------------------
    for (const char* name : { "\\Device\\Harddisk0\\Partition0", "\\device\\harddisk0\\PARTITION0" })
    {
        Opened o = Create(name, GENERIC_READ, FILE_OPEN);
        assert(o.status == X_STATUS_SUCCESS && o.handle && o.information == FILE_OPENED);
        Transfer t = Read(o.handle, 64);               // completes with nothing, not EOF
        assert(t.status == X_STATUS_SUCCESS && t.information == 0);
        t = Write(o.handle, "data");
        assert(t.status == ACCESS_DENIED && t.information == 0);
        CloseHandle(o.handle);
        assert(Create(name, GENERIC_WRITE, FILE_OPEN).status == ACCESS_DENIED);
        assert(Create(name, GENERIC_READ, FILE_OVERWRITE).status == ACCESS_DENIED);
        assert(Create(name, GENERIC_READ, FILE_SUPERSEDE).status == ACCESS_DENIED);
        assert(Create(name, GENERIC_READ, FILE_CREATE).status == X_STATUS_OBJECT_NAME_COLLISION);
        assert(FullAttributes(name) == X_STATUS_SUCCESS);
        assert(uint32_t(*GuestPtr<be32>(kInfo + 48)) == 0x80);
    }
    // Only that one object: a sibling is an ordinary missing file.
    assert(Create("\\Device\\Harddisk0\\Partition1", GENERIC_READ, FILE_OPEN).status ==
           X_STATUS_OBJECT_NAME_NOT_FOUND);

    // ---- a mounted device is writable ------------------------------------------------
    MountDevice("Cache0", saves);                      // names are not case sensitive
    {
        Opened o = Create("cache0:\\profile\\save.dat", GENERIC_READ | GENERIC_WRITE, FILE_OPEN_IF);
        assert(o.status == X_STATUS_SUCCESS && o.information == FILE_CREATED);
        assert(Write(o.handle, "hello").information == 5);
        assert(Write(o.handle, " world", 5).information == 6);
        assert(QueryInfo(o.handle, 5) == X_STATUS_SUCCESS);
        assert(uint64_t(*GuestPtr<be64>(kInfo + 8)) == 11);
        SetInfo(o.handle, 20, 5);                      // FileEndOfFileInformation
        CloseHandle(o.handle);
        assert(Slurp(saves / "profile" / "save.dat") == "hello");

        o = Create("CACHE0:\\Profile\\SAVE.DAT", GENERIC_READ, FILE_OPEN);   // found by any case
        assert(o.status == X_STATUS_SUCCESS && o.information == FILE_OPENED);
        assert(Read(o.handle, 16).information == 5 && Bytes(5) == "hello");
        CloseHandle(o.handle);

        o = Create("cache0:\\profile\\save.dat", GENERIC_WRITE, FILE_OVERWRITE_IF);
        assert(o.information == FILE_OVERWRITTEN);
        CloseHandle(o.handle);
        assert(Slurp(saves / "profile" / "save.dat").empty());

        // Marking a file for deletion removes it when the last handle goes.
        o = Create("cache0:\\profile\\temp.dat", GENERIC_WRITE, FILE_CREATE);
        assert(o.status == X_STATUS_SUCCESS);
        *GuestPtr<uint8_t>(kInfo) = 1;
        {
            PPCContext ctx = Ctx();
            ctx.r3.u64 = o.handle; ctx.r4.u64 = kIosb; ctx.r5.u64 = kInfo; ctx.r7.u64 = 13;
            __imp__NtSetInformationFile(ctx, guest::Base());
        }
        CloseHandle(o.handle);
        assert(!fs::exists(saves / "profile" / "temp.dat"));

        // A directory made under the mount.
        o = Create("cache0:\\profile\\slot1", GENERIC_READ, FILE_OPEN_IF, FILE_DIRECTORY_FILE);
        assert(o.status == X_STATUS_SUCCESS && o.information == FILE_CREATED);
        assert(fs::is_directory(saves / "profile" / "slot1"));
        CloseHandle(o.handle);

        // Nothing written through the mount lands outside it.
        o = Create("cache0:\\..\\..\\escape.txt", GENERIC_WRITE, FILE_CREATE);
        assert(o.status == X_STATUS_SUCCESS);
        CloseHandle(o.handle);
        assert(fs::exists(saves / "escape.txt"));
        assert(!fs::exists(base / "escape.txt") && !fs::exists(base.parent_path() / "escape.txt"));

        // Open-only dispositions still do not create.
        assert(Create("cache0:\\absent.dat", GENERIC_WRITE, FILE_OPEN).status == X_STATUS_OBJECT_NAME_NOT_FOUND);
        assert(Create("cache0:\\absent.dat", GENERIC_WRITE, FILE_OVERWRITE).status == X_STATUS_OBJECT_NAME_NOT_FOUND);
    }
    UnmountDevice("cache0");
    assert(Create("cache0:\\profile\\save.dat", GENERIC_READ, FILE_OPEN).status == X_STATUS_OBJECT_NAME_NOT_FOUND);

    fs::remove_all(base);
    std::puts("kernel-files-test ok");
    return 0;
}
