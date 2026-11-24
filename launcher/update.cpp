// The self-update's C side. The update itself is Rust (launcher/src/update.rs);
// these are update.h's functions over it. Its progress reaches the
// setup::Progress the window draws, and its text the std::strings handed over,
// through setup.cpp's mw2l_setup_* calls, as the install's do.
#include "update.h"

extern "C"
{
    const char* mw2_update_current();
    const char* mw2_update_kind();
    int mw2_update_look(std::string* version, std::string* archive, std::string* sha256, std::string* url,
                        uint64_t* size, std::string* error);
    int mw2_update_install(const char* version, size_t versionLength, const char* archive, size_t archiveLength,
                           const char* sha256, size_t sha256Length, const char* url, size_t urlLength,
                           uint64_t size, setup::Progress* progress, std::string* error);
    void mw2_update_finish(std::string* version, std::string* former);
}

const char* update::Current() { return mw2_update_current(); }
const char* update::Kind() { return mw2_update_kind(); }

update::Check update::Look(Release& release, std::string& error)
{
    return static_cast<Check>(
        mw2_update_look(&release.version, &release.archive, &release.sha256, &release.url, &release.size, &error));
}

setup::Result update::Install(const Release& release, setup::Progress& progress, std::string& error)
{
    return static_cast<setup::Result>(mw2_update_install(
        release.version.data(), release.version.size(), release.archive.data(), release.archive.size(),
        release.sha256.data(), release.sha256.size(), release.url.data(), release.url.size(), release.size,
        &progress, &error));
}

std::string update::Finish(std::string& former)
{
    std::string version;
    mw2_update_finish(&version, &former);
    return version;
}
