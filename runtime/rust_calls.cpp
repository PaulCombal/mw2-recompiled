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
