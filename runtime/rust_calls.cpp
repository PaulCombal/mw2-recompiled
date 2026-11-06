// What the Rust runtime asks of the C++ one (src/runtime/bridge.rs). Built only with it.
#include "crash.h"
#include "gpu/gpu.h"
#include "gpu/memory_watch.h"
#include "gpu/vulkan/pipeline.h"
#include "gpu/vulkan/presenter.h"
#include "gpu/vulkan/renderer.h"
#include "gpu/vulkan/shader_build.h"
#include "text_sink.h"
#if MW2_RUST_RENDERER
#include "gpu/internal.h"
#include "gpu/vulkan/texture_cache.h"
#include "image_move.h"
#if MW2_DIAGNOSTICS
#include "gpu/stream_capture.h"
#endif
#endif

#include <string>

extern "C" void mw2_watch_costs(gpu::watch::Costs* costs) { *costs = gpu::watch::CostsSoFar(); }

extern "C" bool mw2_watch_handle_fault(const void* address) { return gpu::watch::HandleFault(address); }

// gpu.cpp's, which the Rust renderer has in Rust (src/runtime/gpu/gpu.rs).
#if !MW2_RUST_RENDERER
extern "C" uint64_t mw2_gpu_frame_count() { return gpu::FrameCount(); }

extern "C" void mw2_gpu_set_ring_buffer(uint32_t basePhysical, uint32_t sizeLog2) { gpu::SetRingBuffer(basePhysical, sizeLog2); }

extern "C" void mw2_gpu_set_read_pointer_write_back(uint32_t addressPhysical, uint32_t blockSizeLog2)
{
    gpu::SetReadPointerWriteBack(addressPhysical, blockSizeLog2);
}

extern "C" void mw2_gpu_set_interrupt_callback(uint32_t callback, uint32_t context) { gpu::SetInterruptCallback(callback, context); }

extern "C" void mw2_gpu_swap() { gpu::Swap(); }
#endif

extern "C" void mw2_renderer_flash_seen(const char* how) { vk::renderer::FlashSeen(how); }

#if !MW2_RUST_RENDERER
// The start and the end of a run (src/runtime/run.rs).
extern "C" void mw2_gpu_initialise() { gpu::Initialise(); }

extern "C" void mw2_gpu_shutdown() { gpu::Shutdown(); }

extern "C" void mw2_gpu_report() { gpu::Report(); }
#endif

extern "C" void mw2_window_show() { vk::ShowWindow(); }

// A player's start draws only with the shaders the launcher compiled for the device.
extern "C" void mw2_shaders_use_compiled(const char* gameFolder)
{
    vk::pipeline::SetCachePath(vk::shaders::kCacheFile);
    vk::renderer::CheckCompiledShaders(gameFolder);
}

extern "C" bool mw2_shaders_compiled(void* why, void* device, mw2_text_sink sink)
{
    std::string whyText, deviceText;
    const bool compiled = vk::renderer::CompiledShaders(whyText, deviceText);
    sink(why, whyText.data(), whyText.size());
    sink(device, deviceText.data(), deviceText.size());
    return compiled;
}

#if MW2_RUST_RENDERER
// The Rust renderer's calls into the C++ pipeline.cpp, which stays until the launcher
// moves (src/runtime/bridge.rs, vulkan).
extern "C" bool mw2_vk_device_has_extension(void* physical, const char* name)
{
    return vk::pipeline::DeviceHasExtension(physical, name);
}

extern "C" bool mw2_vk_create_device(void* physical, uint32_t family, const char* const* extensions, uint32_t count,
                                     void** device, void** queue, void* features)
{
    return vk::pipeline::CreateDevice(physical, family, extensions, count, device, queue, features);
}

extern "C" bool mw2_vk_pipeline_initialise(void* device, void* physical, void* queue, uint32_t family, void* instance)
{
    return vk::pipeline::Initialise(device, physical, queue, family, instance);
}

extern "C" bool mw2_vk_failed(int result, const char* where) { return vk::pipeline::Failed(result, where); }

extern "C" void mw2_vk_queue_lock() { vk::pipeline::QueueMutex().lock(); }

extern "C" void mw2_vk_queue_unlock() { vk::pipeline::QueueMutex().unlock(); }

extern "C" void* mw2_vk_instance() { return vk::pipeline::Instance(); }

extern "C" void* mw2_vk_queue() { return vk::pipeline::Queue(); }

extern "C" void* mw2_vk_physical() { return vk::pipeline::PhysicalDevice(); }

// What gpu/gpu.rs and gpu/arena.rs call of the C++ renderer that is left
// (src/runtime/bridge.rs, gpu): the command processor, the renderer's start, end, idle
// and reports, the census, the image pool, and the GPU capture's brackets.
extern "C" void mw2_cp_execute_ring(uint32_t base, uint32_t readIndex, uint32_t mask, uint32_t dwords)
{
    gpu::detail::ExecuteRing(base, readIndex, mask, dwords);
}

extern "C" void mw2_cp_deliver_occlusion_queries() { gpu::detail::DeliverOcclusionQueries(); }

extern "C" bool mw2_cp_rendering() { return gpu::detail::Rendering(); }

extern "C" void mw2_cp_write_physical(uint32_t physical, uint32_t value) { gpu::detail::WritePhysical(physical, value); }

extern "C" bool mw2_renderer_initialise() { return vk::renderer::Initialise(); }

extern "C" void mw2_renderer_shutdown() { vk::renderer::Shutdown(); }

extern "C" void mw2_renderer_idle() { vk::renderer::Idle(); }

extern "C" void mw2_textures_shutdown() { vk::textures::Shutdown(); }

extern "C" void mw2_pipeline_shutdown() { vk::pipeline::Shutdown(); }

#if MW2_DIAGNOSTICS
extern "C" void mw2_cp_report() { gpu::detail::ReportCommandProcessor(); }

extern "C" void mw2_census_present() { gpu::detail::CensusPresent(); }

extern "C" void mw2_census_report() { gpu::detail::ReportCensus(); }

extern "C" void mw2_renderer_report() { vk::renderer::Report(); }

extern "C" void mw2_textures_report() { vk::textures::Report(); }

extern "C" void mw2_watch_report() { gpu::watch::Report(); }

extern "C" void mw2_imagepool_report() { imagepool::Report(); }

extern "C" void mw2_stream_batch_begin(uint32_t base, uint32_t readIndex, uint32_t mask, uint32_t dwords)
{
    gpu::stream::BatchBegin(base, readIndex, mask, dwords);
}

extern "C" void mw2_stream_batch_end() { gpu::stream::BatchEnd(); }

extern "C" void mw2_stream_shutdown() { gpu::stream::Shutdown(); }

extern "C" void mw2_stream_report() { gpu::stream::Report(); }

extern "C" void mw2_stream_arena_block_seen(uint32_t block)
{
    if (gpu::stream::Capturing()) gpu::stream::ArenaBlockSeen(block);
}
#endif
#endif
