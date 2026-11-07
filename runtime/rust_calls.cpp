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
#include "gpu/vulkan/trace.h"
#endif
#endif

#include <string>

// The memory watch's, which the Rust renderer has in Rust (src/runtime/gpu/memory_watch.rs).
#if !MW2_OWNS_MEMORY_WATCH
extern "C" void mw2_watch_costs(gpu::watch::Costs* costs) { *costs = gpu::watch::CostsSoFar(); }

extern "C" bool mw2_watch_handle_fault(const void* address) { return gpu::watch::HandleFault(address); }
#endif

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
// (src/runtime/bridge.rs, gpu): the command processor, census and image pool while they
// are C++ (MW2_OWNS_*, from build.rs's RUST_OWNS, says which Rust has), the renderer's
// start, end, idle and reports, and the GPU capture's brackets.
#if !MW2_OWNS_COMMAND_PROCESSOR
extern "C" void mw2_cp_execute_ring(uint32_t base, uint32_t readIndex, uint32_t mask, uint32_t dwords)
{
    gpu::detail::ExecuteRing(base, readIndex, mask, dwords);
}

extern "C" void mw2_cp_deliver_occlusion_queries() { gpu::detail::DeliverOcclusionQueries(); }

extern "C" bool mw2_cp_rendering() { return gpu::detail::Rendering(); }

extern "C" void mw2_cp_write_physical(uint32_t physical, uint32_t value) { gpu::detail::WritePhysical(physical, value); }
#endif

extern "C" bool mw2_renderer_initialise() { return vk::renderer::Initialise(); }

extern "C" void mw2_renderer_shutdown() { vk::renderer::Shutdown(); }

extern "C" void mw2_renderer_idle() { vk::renderer::Idle(); }

extern "C" void mw2_textures_shutdown() { vk::textures::Shutdown(); }

extern "C" void mw2_pipeline_shutdown() { vk::pipeline::Shutdown(); }

#if MW2_DIAGNOSTICS
#if !MW2_OWNS_COMMAND_PROCESSOR
extern "C" void mw2_cp_report() { gpu::detail::ReportCommandProcessor(); }
#endif

#if !MW2_OWNS_CENSUS
extern "C" void mw2_census_present() { gpu::detail::CensusPresent(); }

extern "C" void mw2_census_report() { gpu::detail::ReportCensus(); }
#endif

extern "C" void mw2_renderer_report() { vk::renderer::Report(); }

extern "C" void mw2_textures_report() { vk::textures::Report(); }

#if !MW2_OWNS_MEMORY_WATCH
extern "C" void mw2_watch_report() { gpu::watch::Report(); }
#endif

#if !MW2_OWNS_IMAGE_MOVE
extern "C" void mw2_imagepool_report() { imagepool::Report(); }
#endif

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

// What gpu/command_processor.rs and gpu/image_move.rs call of the C++ renderer, the
// memory watch and the GPU capture (src/runtime/bridge.rs: renderer, watch, stream). The
// register file and the draw cross by pointer, their layouts asserted here and in Rust.
#include "gpu/registers.h"

#include <cstddef>

static_assert(offsetof(gpu::RegisterFile, values) == 0 && offsetof(gpu::RegisterFile, stamps) == 81936 &&
              offsetof(gpu::RegisterFile, writes) == 84504 && sizeof(gpu::RegisterFile) == 84512);
static_assert(offsetof(vk::renderer::DrawCall, primitive) == 0 && offsetof(vk::renderer::DrawCall, indexCount) == 4 &&
              offsetof(vk::renderer::DrawCall, indexed) == 8 && offsetof(vk::renderer::DrawCall, index32) == 9 &&
              offsetof(vk::renderer::DrawCall, indexAddress) == 12 &&
              offsetof(vk::renderer::DrawCall, vertexCode) == 16 &&
              offsetof(vk::renderer::DrawCall, vertexWords) == 24 &&
              offsetof(vk::renderer::DrawCall, vertexHash) == 32 && offsetof(vk::renderer::DrawCall, pixelCode) == 40 &&
              offsetof(vk::renderer::DrawCall, pixelWords) == 48 && offsetof(vk::renderer::DrawCall, pixelHash) == 56 &&
              sizeof(vk::renderer::DrawCall) == 64);

extern "C" void mw2_renderer_draw(const gpu::RegisterFile* registers, const vk::renderer::DrawCall* call)
{
    vk::renderer::Draw(*registers, *call);
}

extern "C" void mw2_renderer_resolve(const gpu::RegisterFile* registers) { vk::renderer::Resolve(*registers); }

extern "C" void mw2_renderer_swap(uint32_t frontBuffer) { vk::renderer::Swap(frontBuffer); }

extern "C" void mw2_renderer_before_completion() { vk::renderer::BeforeCompletion(); }

extern "C" void mw2_renderer_begin_occlusion() { vk::renderer::BeginOcclusionQuery(); }

extern "C" bool mw2_renderer_end_occlusion(uint32_t tag, uint64_t* samples)
{
    return vk::renderer::EndOcclusionQuery(tag, *samples);
}

extern "C" void mw2_renderer_collect_occlusion(void (*deliver)(uint32_t tag, uint64_t samples))
{
    vk::renderer::CollectOcclusionQueries(deliver);
}

extern "C" void mw2_renderer_before_stream_write(uint32_t physical, uint32_t size)
{
    vk::renderer::BeforeStreamWrite(physical, size);
}

extern "C" void mw2_textures_memory_written(uint32_t physical, uint32_t size) { vk::textures::MemoryWritten(physical, size); }

#if !MW2_OWNS_MEMORY_WATCH
// The memory watch, C++ while Rust does not own it.
extern "C" bool mw2_watch_enabled() { return gpu::watch::Enabled(); }

extern "C" uint64_t mw2_watch_track(uint32_t physical, uint32_t bytes, uint64_t frame)
{
    return gpu::watch::Track(physical, bytes, vk::renderer::CompletedSubmission, frame);
}

extern "C" bool mw2_watch_unwritten(uint32_t physical, uint32_t bytes, uint64_t since)
{
    return gpu::watch::Unwritten(physical, bytes, since);
}

extern "C" void mw2_watch_will_write(uint32_t address, uint32_t bytes) { gpu::watch::WillWrite(address, bytes); }
#else
// What gpu/memory_watch.rs asks of the renderer: the last submission the GPU has finished
// (a cached number), which the shader memo's Track is given.
extern "C" uint64_t mw2_renderer_completed_submission() { return vk::renderer::CompletedSubmission(); }

#if MW2_DIAGNOSTICS
// The command trace's line for each page the shadow copies.
extern "C" void mw2_trace_shadow_arm(uint32_t first, uint32_t count, const uint8_t* shadow)
{
#if MW2_TRACE
    if (vk::trace::Wanted())
        for (uint32_t p = first; p < first + count; p++)
            vk::trace::Note("ShadowArm page=%u fnv=%016llx", p,
                            (unsigned long long)vk::trace::FnvWriteCombined(shadow + size_t(p) * gpu::watch::kPageBytes,
                                                                            gpu::watch::kPageBytes));
#endif
}
#endif
#endif

#if MW2_OWNS_SHADER_PRELOAD
// A shader the title loaded, to compile before its first draw (src/hooks/shader_preload.rs).
extern "C" void mw2_renderer_prepare_shader(bool pixel, const uint32_t* code, size_t words)
{
    vk::renderer::PrepareShader(pixel, code, words);
}
#endif

#if MW2_DIAGNOSTICS
// The capture's three flags, by address: the Rust reads them where the C++ reads them inline.
extern "C" const bool* const mw2_stream_capturing = &gpu::stream::detail::capturing;
extern "C" const bool* const mw2_stream_replaying = &gpu::stream::detail::replaying;
extern "C" const bool* const mw2_stream_counting = &gpu::stream::detail::counting;

extern "C" void mw2_stream_packet() { gpu::stream::PacketSlow(); }

extern "C" void mw2_stream_read(uint32_t address, uint64_t bytes) { gpu::stream::ReadSlow(address, bytes); }

extern "C" void mw2_stream_polled(uint32_t address) { gpu::stream::PolledSlow(address); }

extern "C" void mw2_stream_write(uint32_t address, uint32_t bytes, bool done) { gpu::stream::WriteSlow(address, bytes, done); }

extern "C" void mw2_stream_aperture_read(uint32_t index, uint32_t value) { gpu::stream::ApertureRead(index, value); }

extern "C" void mw2_stream_wait_done(bool memory, uint32_t address, uint32_t value, uint32_t polls, bool timedOut)
{
    gpu::stream::WaitDone(memory, address, value, polls, timedOut);
}

static_assert(sizeof(gpu::stream::Wait) == 16 && offsetof(gpu::stream::Wait, value) == 4 &&
              offsetof(gpu::stream::Wait, polls) == 8 && offsetof(gpu::stream::Wait, timedOut) == 12);

extern "C" bool mw2_stream_replay_wait(gpu::stream::Wait* wait) { return gpu::stream::ReplayWait(*wait); }

extern "C" void mw2_stream_pool_seen(uint32_t destination, uint32_t source, uint32_t size, uint32_t streamConstant,
                                     uint64_t frame)
{
    gpu::stream::PoolSeen(destination, source, size, streamConstant, frame);
}

extern "C" void mw2_stream_diverged(const char* what) { gpu::stream::Diverged(what); }
#endif
#endif
