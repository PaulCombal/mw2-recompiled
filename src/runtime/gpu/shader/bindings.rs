// The interface every translated shader is built against. The translator
// declares its resources here, the pipeline layout is made from the same
// numbers, and the renderer fills them -- so one layout serves every shader
// and nothing is reflected at bind time.
//
// Nothing in it changes from one draw to the next but where the draw's record
// is (RECORD_*): both sets are bound once a command buffer, and the layout has
// no push constants. Per draw, the driver is handed a pipeline when it changes,
// the dynamic state that changed, and the draw itself. A push constant range
// written at every draw was a third of the recorder thread's time in RADV,
// which re-reads every bound shader's description to place the values; a
// texture set bound at every draw was another tenth.
//
// The same numbers as runtime/gpu/vulkan/bindings.h, which stays for the C++
// renderer; tools/shader_diff checks the two agree.

// Five blocks of constants, each copied from the register file into the
// frame arena (set 2) and found there by the dword offset the draw's
// record holds for it (RECORD_OFFSETS). Set 0 has nothing in it.
pub const CONSTANT_SET: u32 = 0;
pub const VERTEX_FLOATS: u32 = 0; // the vertex program's constant table: a uvec4
// whose x is SQ_VS_CONST's base, then for each of
// the file's 32 blocks of 16 vec4 the vec4 index
// of its copy in the arena (ARENA_FLOATS)
pub const FETCH_CONSTANTS: u32 = 1; // 96 vertex fetch constants, two dwords
// each, padded to a uvec4
pub const BOOL_CONSTANTS: u32 = 2; // 256 bits as 8 dwords
pub const LOOP_CONSTANTS: u32 = 3; // 32 dwords
pub const PIXEL_FLOATS: u32 = 4; // 256 vec4 from SQ_PS_CONST's base
pub const CONSTANT_BLOCKS: u32 = 5;
pub const FLOAT_CONSTANTS: u32 = 256;
pub const CONSTANT_FILE_BLOCKS: u32 = 32; // 512 vec4, 16 a block
pub const CONSTANT_TABLE_WORDS: u32 = 4 + CONSTANT_FILE_BLOCKS;
pub const FETCH_SLOTS: u32 = 96;

// Set 1: every texture the cache holds, and every sampler. One array of
// sampled images, indexed by the texture's place in it, and one of
// samplers; the draw's record says, for each fetch slot, which image and
// which sampler (RECORD_TEXTURES). A shader declares the image array once
// for each kind it samples a slot as -- 2D, a cube map, a volume -- all at
// the same binding: Vulkan lets variables share a binding when each is of
// a type its descriptor type allows, and a sampled image's descriptor is
// the same whatever its view's kind. An element is only read as the kind
// its view was made as: elements 0 to 2 are 1x1 stand-ins, one of each
// kind in its number's place, for a slot with nothing bound or with a
// texture of another kind, and every other element holds one texture. An
// entry is written once, when the texture is made, and the set stays
// bound.
pub const TEXTURE_SET: u32 = 1;
pub const TEXTURE_SLOTS: u32 = 32;
pub const IMAGES: u32 = 0;
pub const SAMPLERS: u32 = 1;
pub const IMAGE_KINDS: u32 = 3; // 2D, cube, volume: the stand-ins
pub const IMAGE_COUNT: u32 = 65536;
pub const SAMPLER_COUNT: u32 = 4096;
// A slot's word in the record: the image's index, then the sampler's.
pub const HANDLE_IMAGE_BITS: u32 = 16;

// Set 2: the frame arena as an array of dwords, where the constant blocks
// are, and what the draw's vertex fetches read: a vertex fetch constant
// holds a guest physical address; the renderer copies what the draw reads
// into the arena and rewrites the constant to point at the copy.
pub const ARENA_SET: u32 = 2;
pub const ARENA_BINDING: u32 = 0;
// The same buffer as vec4, for the float constants.
pub const ARENA_FLOATS: u32 = 1;

// A draw's record: what is register state rather than shader state, in the
// arena, at the dword its draw names as its base vertex (vertexOffset, or
// firstVertex for a draw without indices). The vertex stage reads it
// through BaseVertex, which is the same for the whole draw and so is one
// scalar register on the GPU, and takes it away from the vertex index; it
// hands it to the pixel stage as a flat value (location RECORD_LOCATION),
// which the pixel stage makes uniform again across its subgroup. A
// subgroup holds one draw's invocations -- an index into the texture
// arrays without NonUniform already counts on that -- and without it every
// constant read in a pixel shader would be a load per pixel. The base
// vertex rather than the first instance, because a run of draws made with
// one vkCmdDrawMulti*EXT shares its first instance and not its offsets.
//
// { uint x5 } the dword offset of each constant block, in the order above.
pub const RECORD_OFFSETS: u32 = 0;
// Vertex: { float2 scale, float2 offset }, window coordinates to clip space
// for a program drawn with the viewport transform off.
pub const RECORD_NDC: u32 = 5;
// Pixel: { uint alpha function, float alpha reference, float colour scale,
//          uint the target is the gamma form }.
pub const RECORD_PIXEL: u32 = 9;
// Both, read only by shaders built for render targets larger than the
// title's (MW2_SCALE, MW2_SHADOW_SCALE): { uint frame copies, uint shadow
// copies, float frame factor, float shadow factor }. A bit per texture
// slot bound to a resolve's copy, whose image is a whole number of times
// the size the title gave it -- the frame's scale, or the shadow maps' --
// and the reciprocal of each scale.
pub const RECORD_SCALED: u32 = 13;
// Pixel: a word per fetch slot, up to the highest the pixel shader
// samples (HANDLE_IMAGE_BITS).
pub const RECORD_TEXTURES: u32 = 17;
// Vertex: the same for the vertex shader's slots, present only when it
// samples any. A stage's own, because the two may declare one slot as
// different kinds, and an element of set 1 is read as one kind only.
pub const RECORD_VERTEX_TEXTURES: u32 = RECORD_TEXTURES + TEXTURE_SLOTS;
pub const RECORD_WORDS: u32 = RECORD_VERTEX_TEXTURES + TEXTURE_SLOTS;
// After the title's sixteen interpolators.
pub const RECORD_LOCATION: u32 = 16;

// Every constant above by its C++ name, for the rig that holds the two
// copies to one another.
pub const ALL: [(&str, u32); 30] = [
    ("kConstantSet", CONSTANT_SET),
    ("kVertexFloats", VERTEX_FLOATS),
    ("kFetchConstants", FETCH_CONSTANTS),
    ("kBoolConstants", BOOL_CONSTANTS),
    ("kLoopConstants", LOOP_CONSTANTS),
    ("kPixelFloats", PIXEL_FLOATS),
    ("kConstantBlocks", CONSTANT_BLOCKS),
    ("kFloatConstants", FLOAT_CONSTANTS),
    ("kConstantFileBlocks", CONSTANT_FILE_BLOCKS),
    ("kConstantTableWords", CONSTANT_TABLE_WORDS),
    ("kFetchSlots", FETCH_SLOTS),
    ("kTextureSet", TEXTURE_SET),
    ("kTextureSlots", TEXTURE_SLOTS),
    ("kImages", IMAGES),
    ("kSamplers", SAMPLERS),
    ("kImageKinds", IMAGE_KINDS),
    ("kImageCount", IMAGE_COUNT),
    ("kSamplerCount", SAMPLER_COUNT),
    ("kHandleImageBits", HANDLE_IMAGE_BITS),
    ("kArenaSet", ARENA_SET),
    ("kArenaBinding", ARENA_BINDING),
    ("kArenaFloats", ARENA_FLOATS),
    ("kRecordOffsets", RECORD_OFFSETS),
    ("kRecordNdc", RECORD_NDC),
    ("kRecordPixel", RECORD_PIXEL),
    ("kRecordScaled", RECORD_SCALED),
    ("kRecordTextures", RECORD_TEXTURES),
    ("kRecordVertexTextures", RECORD_VERTEX_TEXTURES),
    ("kRecordWords", RECORD_WORDS),
    ("kRecordLocation", RECORD_LOCATION),
];
