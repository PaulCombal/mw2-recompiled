// What the game and the launcher share to build their shaders the same way.
//
// - shader/: the translator, Xenos microcode to SPIR-V (docs/shaders.md);
// - pipeline.rs: the Vulkan device the renderer draws on, its features and limits, the
//   pipeline layout every translated shader is built against, and the pipeline cache;
// - shader_build.rs: one shader made ready for the GPU, its module and its stage's
//   pipeline library, and the state every pipeline starts from;
// - marker.rs: the marker the launcher's compile pass writes and the game reads.
//
// A driver finds a shader in the pipeline cache only when it is asked for exactly what
// was put there, so the launcher's compile pass and the game build every shader here.
// Everything in this crate is in the shader build id (build.rs).

pub mod marker;
pub mod pipeline;
pub mod shader;
pub mod shader_build;

mod host;
pub use host::set_log;
// std::filesystem's rename and remove, as the C++ launcher's pass and game had them.
pub use host::files;
