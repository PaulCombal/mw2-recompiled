// The game's fastfiles as its loader streams them:
//
// - fastfile.rs: a 360 fastfile's zone, inflated;
// - defs.rs: the structures of a zone, from defs/iw4_assets.h, laid out as the 360's
//   compiler lays them out;
// - expr.rs: the definitions' integer expressions;
// - schema.rs and rules.rs: how the title streams them, from defs/commands.txt and
//   defs/assets/;
// - walker.rs: every asset of a zone, walked in the loader's order;
// - shaders.rs and vertex_binding.rs: every shader of a zone and of an executable, as
//   the renderer will be handed it, vertex shaders bound as the title binds them.
//
// The launcher's compile pass lists the game's shaders with it, so everything in src/
// and defs/ is in the shader build id (shaders/build.rs). Nothing here panics on what a
// file holds: a damaged file is an Error, whose text the launcher shows.
#![deny(unsafe_code)]

pub mod defs;
pub mod expr;
pub mod fastfile;
pub mod rules;
pub mod schema;
pub mod shaders;
pub mod vertex_binding;
pub mod walker;

/// What went wrong reading a file or the definitions, as the player is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl Error {
    pub fn new(text: impl Into<String>) -> Error {
        Error(text.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
