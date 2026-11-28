// The map converter: a map made for the PC game made into one the 360's multiplayer loads.
#![deny(unsafe_code)]

pub mod convert;
pub mod decals;
pub mod deflate;
pub mod error;
pub mod fastfile;
pub mod images;
pub mod materials;
pub mod num;
pub mod ordered;
pub mod paths;
pub mod pictures;
pub mod port;
pub mod porter;
pub mod query;
pub mod reader;
pub mod rules;
pub mod sounds;
pub mod stock;
pub mod teams;
pub mod text;
pub mod tree;
pub mod wavelet;
pub mod world;
pub mod xma;
pub mod zip;

pub use error::{Error, Result};
