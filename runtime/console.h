#pragma once
struct PPCContext;

extern "C"
{
    void mw2_console_pump(PPCContext& ctx, unsigned char* base);
    void mw2_console_run_now(const char* text);
    void mw2_console_place(const char* text);
}

// Queues commands from MW2_CONSOLE into the title's command buffer. Call from
// a hook that runs on a guest thread every frame; it does nothing until one of
// the scripted times has passed, and nothing at all without MW2_CONSOLE.
namespace console
{
    inline void Pump(PPCContext& ctx, unsigned char* base) { mw2_console_pump(ctx, base); }
    // Queues one command for the next pump, from any thread -- a key press.
    inline void RunNow(const char* text) { mw2_console_run_now(text); }
    // The same for a command sent every frame, a camera being moved: the one
    // waiting is replaced instead of queued behind, and none of them is logged.
    inline void Place(const char* text) { mw2_console_place(text); }
}
