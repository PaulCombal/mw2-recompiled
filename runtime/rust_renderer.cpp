// The C++ renderer's calls into what Rust has taken over of it (build.rs, RUST_OWNS):
// each function is the C++ declaration of its name, defined here to call the Rust
// (src/runtime/gpu/exports.rs), so that no header and no caller changes. Compiled only
// with the rust-renderer feature; CMake's build never sees it.
#if MW2_RUST_RENDERER
#endif
