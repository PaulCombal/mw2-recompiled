// The install's C side. The install itself is Rust (launcher/src/setup.rs), and
// so is the one download it makes (launcher/src/download.rs); these are setup.h's
// functions over it, and what it calls back to report its progress to the
// setup::Progress the window draws and to hand its error back. The self-update
// (launcher/src/update.rs) reports through the same calls.
//
// Text crosses as bytes and a length, paths as UTF-8 (install::Utf8). Nothing is
// thrown across: each function here is noexcept, so memory running out while a
// few hundred bytes of text are copied ends the program, as it would have
// anywhere else in the launcher.
#include "setup.h"
#include "utf8.h"

namespace fs = std::filesystem;

extern "C"
{
    // ---- what setup.rs calls -------------------------------------------------
    // The steps a run takes, none taken yet.
    void mw2l_setup_begin(setup::Progress* progress, int steps) noexcept
    {
        std::lock_guard lock(progress->lock);
        progress->step = 0;
        progress->steps = steps;
    }
    // A step begun: its title, nothing copied yet.
    void mw2l_setup_step(setup::Progress* progress, int step, const char* title, size_t length) noexcept
    {
        std::lock_guard lock(progress->lock);
        progress->step = step;
        progress->title.assign(title, length);
        progress->detail.clear();
        progress->done = progress->total = 0;
    }
    void mw2l_setup_detail(setup::Progress* progress, const char* text, size_t length) noexcept
    {
        std::lock_guard lock(progress->lock);
        progress->detail.assign(text, length);
    }
    void mw2l_setup_total(setup::Progress* progress, uint64_t total) noexcept
    {
        std::lock_guard lock(progress->lock);
        progress->total = total;
    }
    void mw2l_setup_add(setup::Progress* progress, uint64_t done) noexcept
    {
        std::lock_guard lock(progress->lock);
        progress->done += done;
    }
    bool mw2l_setup_cancelled(setup::Progress* progress) noexcept { return progress->cancel; }
    void mw2l_setup_text(std::string* to, const char* text, size_t length) noexcept { to->assign(text, length); }

    // ---- setup.rs ----------------------------------------------------------------
    int mw2_setup_detect();
    int mw2_setup_run(const char* disc, size_t discLength, const char* update, size_t updateLength,
                      setup::Progress* progress, std::string* error);
    const char* mw2_setup_update_url(size_t* length);
    bool mw2_setup_uses_update();
}

fs::path setup::GameFolder() { return "game"; }

std::string setup::UpdateUrl()
{
    size_t length = 0;
    const char* url = mw2_setup_update_url(&length);
    return std::string(url, length);
}

bool setup::UsesUpdate() { return mw2_setup_uses_update(); }

setup::State setup::Detect() { return static_cast<State>(mw2_setup_detect()); }

setup::Result setup::Run(const fs::path& disc, const fs::path& update, Progress& progress, std::string& error)
{
    const std::string from = install::Utf8(disc), with = install::Utf8(update);
    return static_cast<Result>(mw2_setup_run(from.data(), from.size(), with.data(), with.size(), &progress, &error));
}
