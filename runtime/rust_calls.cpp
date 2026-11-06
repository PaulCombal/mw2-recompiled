// What the Rust runtime asks of the C++ one (src/runtime/bridge.rs). Built only with it.
#include "crash.h"
#include "gpu/gpu.h"
#include "gpu/memory_watch.h"
#include "gpu/vulkan/pipeline.h"
#include "gpu/vulkan/presenter.h"
#include "gpu/vulkan/renderer.h"
#include "gpu/vulkan/shader_build.h"
#include "text_sink.h"

#include <string>

extern "C" void mw2_watch_costs(gpu::watch::Costs* costs) { *costs = gpu::watch::CostsSoFar(); }

extern "C" bool mw2_watch_handle_fault(const void* address) { return gpu::watch::HandleFault(address); }

extern "C" uint64_t mw2_gpu_frame_count() { return gpu::FrameCount(); }

extern "C" void mw2_gpu_set_ring_buffer(uint32_t basePhysical, uint32_t sizeLog2) { gpu::SetRingBuffer(basePhysical, sizeLog2); }

extern "C" void mw2_gpu_set_read_pointer_write_back(uint32_t addressPhysical, uint32_t blockSizeLog2)
{
    gpu::SetReadPointerWriteBack(addressPhysical, blockSizeLog2);
}

extern "C" void mw2_gpu_set_interrupt_callback(uint32_t callback, uint32_t context) { gpu::SetInterruptCallback(callback, context); }

extern "C" void mw2_gpu_swap() { gpu::Swap(); }

extern "C" void mw2_renderer_flash_seen(const char* how) { vk::renderer::FlashSeen(how); }

// The start and the end of a run (src/runtime/run.rs).
extern "C" void mw2_gpu_initialise() { gpu::Initialise(); }

extern "C" void mw2_gpu_shutdown() { gpu::Shutdown(); }

extern "C" void mw2_gpu_report() { gpu::Report(); }

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
#endif
