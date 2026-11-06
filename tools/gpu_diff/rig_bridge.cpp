// What the Rust renderer calls of the C++ pipeline and the rest of the runtime
// (src/runtime/bridge.rs), for the rigs that link the Rust renderer without the game:
// Rust calls through the GOT, so each is bound when a rig starts, and no rig reaches one
// but the weak ones, which the presenter rig defines (presenter_cpp.cpp).
#include <cstdint>
#include <cstdlib>

extern "C"
{
    bool mw2_vk_device_has_extension(void*, const char*) { std::abort(); }
    bool mw2_vk_create_device(void*, uint32_t, const char* const*, uint32_t, void**, void**, void*) { std::abort(); }
    bool mw2_vk_pipeline_initialise(void*, void*, void*, uint32_t, void*) { std::abort(); }
    __attribute__((weak)) bool mw2_vk_failed(int, const char*) { std::abort(); }
    __attribute__((weak)) void mw2_vk_queue_lock() { std::abort(); }
    __attribute__((weak)) void mw2_vk_queue_unlock() { std::abort(); }
    void* mw2_vk_instance() { std::abort(); }
    void mw2_crash_request_exit(const char*) { std::abort(); }
    void mw2_renderer_flash_seen(const char*) { std::abort(); }
    void rig_xam_told(const char*, uint32_t, uint32_t) { std::abort(); }
}
