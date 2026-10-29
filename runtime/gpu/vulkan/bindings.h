#pragma once
// The interface every translated shader is built against. The translator
// declares its resources here, the pipeline layout is made from the same
// numbers, and the renderer fills them -- so one layout serves every shader
// and nothing is reflected at bind time.
//
// Nothing in it changes from one draw to the next but where the draw's record
// is (kRecord*): both sets are bound once a command buffer, and the layout has
// no push constants. Per draw, the driver is handed a pipeline when it changes,
// the dynamic state that changed, and the draw itself. A push constant range
// written at every draw was a third of the recorder thread's time in RADV,
// which re-reads every bound shader's description to place the values; a
// texture set bound at every draw was another tenth.
#include <cstdint>

namespace vk::bindings
{
    // Five blocks of constants, each copied from the register file into the
    // frame arena (set 2) and found there by the dword offset the draw's
    // record holds for it (kRecordOffsets). Set 0 has nothing in it.
    constexpr uint32_t kConstantSet = 0;
    constexpr uint32_t kVertexFloats = 0;     // the vertex program's constant table: a uvec4
                                              // whose x is SQ_VS_CONST's base, then for each of
                                              // the file's 32 blocks of 16 vec4 the vec4 index
                                              // of its copy in the arena (kArenaFloats)
    constexpr uint32_t kFetchConstants = 1;   // 96 vertex fetch constants, two dwords
                                              // each, padded to a uvec4
    constexpr uint32_t kBoolConstants = 2;    // 256 bits as 8 dwords
    constexpr uint32_t kLoopConstants = 3;    // 32 dwords
    constexpr uint32_t kPixelFloats = 4;      // 256 vec4 from SQ_PS_CONST's base
    constexpr uint32_t kConstantBlocks = 5;
    constexpr uint32_t kFloatConstants = 256;
    constexpr uint32_t kConstantFileBlocks = 32;  // 512 vec4, 16 a block
    constexpr uint32_t kConstantTableWords = 4 + kConstantFileBlocks;
    constexpr uint32_t kFetchSlots = 96;

    // Set 1: every texture the cache holds, and every sampler. A sampled image
    // array for each kind a shader declares a slot as -- 2D, a cube map, a
    // volume -- indexed by the texture's id, and one array of samplers; the
    // draw's record says, for each fetch slot, which image and which sampler
    // (kRecordTextures). Element 0 of each image array is a 1x1 stand-in, for
    // a slot with nothing bound or with a texture of another kind. An entry is
    // written once, when the texture is made, and the set stays bound.
    constexpr uint32_t kTextureSet = 1;
    constexpr uint32_t kTextureSlots = 32;
    constexpr uint32_t kImages2D = 0, kImagesCube = 1, kImages3D = 2, kSamplers = 3;
    constexpr uint32_t kImageKinds = 3;          // the binding is the kind
    constexpr uint32_t kImageCount = 65536;      // each kind's array; ids are below this
    constexpr uint32_t kSamplerCount = 4096;
    // A slot's word in the record: the image's index, then the sampler's.
    constexpr uint32_t kHandleImageBits = 16;

    // Set 2: the frame arena as an array of dwords, where the constant blocks
    // are, and what the draw's vertex fetches read: a vertex fetch constant
    // holds a guest physical address; the renderer copies what the draw reads
    // into the arena and rewrites the constant to point at the copy.
    constexpr uint32_t kArenaSet = 2;
    constexpr uint32_t kArenaBinding = 0;
    // The same buffer as vec4, for the float constants.
    constexpr uint32_t kArenaFloats = 1;

    // A draw's record: what is register state rather than shader state, in the
    // arena, at the dword its draw names as its base vertex (vertexOffset, or
    // firstVertex for a draw without indices). The vertex stage reads it
    // through BaseVertex, which is the same for the whole draw and so is one
    // scalar register on the GPU, and takes it away from the vertex index; it
    // hands it to the pixel stage as a flat value (location kRecordLocation),
    // which the pixel stage makes uniform again across its subgroup. A
    // subgroup holds one draw's invocations -- an index into the texture
    // arrays without NonUniform already counts on that -- and without it every
    // constant read in a pixel shader would be a load per pixel. The base
    // vertex rather than the first instance, because a run of draws made with
    // one vkCmdDrawMulti*EXT shares its first instance and not its offsets.
    //
    // { uint x5 } the dword offset of each constant block, in the order above.
    constexpr uint32_t kRecordOffsets = 0;
    // Vertex: { float2 scale, float2 offset }, window coordinates to clip space
    // for a program drawn with the viewport transform off.
    constexpr uint32_t kRecordNdc = 5;
    // Pixel: { uint alpha function, float alpha reference, float colour scale,
    //          uint the target is the gamma form }.
    constexpr uint32_t kRecordPixel = 9;
    // Both, read only by shaders built for render targets larger than the
    // title's (MW2_SCALE, MW2_SHADOW_SCALE): { uint frame copies, uint shadow
    // copies, float frame factor, float shadow factor }. A bit per texture
    // slot bound to a resolve's copy, whose image is a whole number of times
    // the size the title gave it -- the frame's scale, or the shadow maps' --
    // and the reciprocal of each scale.
    constexpr uint32_t kRecordScaled = 13;
    // Both: a word per fetch slot, up to the highest either shader samples
    // (kHandleImageBits).
    constexpr uint32_t kRecordTextures = 17;
    constexpr uint32_t kRecordWords = kRecordTextures + kTextureSlots;
    // After the title's sixteen interpolators.
    constexpr uint32_t kRecordLocation = 16;
}
