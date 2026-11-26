// The loading rules the launcher carries (defs/, embedded by build.rs), read once: the
// 360 title update's, which the game's files are read by.

use std::sync::OnceLock;

use crate::schema::Schema;
use crate::{Error, Result};

include!(concat!(env!("OUT_DIR"), "/definitions.rs"));

/// An embedded definition file by name ("assets/Material.txt").
pub fn definition(name: &str) -> Result<&'static str> {
    DEFINITIONS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, text)| *text)
        .ok_or_else(|| Error::new(format!("the launcher has no definition file {name}")))
}

/// The rules of `header`, `commands`, and the folder the asset rules they include are
/// in, read by `file`: the rules include "XAssets/<asset>.txt", which is
/// assets/<asset>.txt here.
pub fn load(file: &dyn Fn(&str) -> Result<String>) -> Result<Schema> {
    Schema::new(&file("iw4_assets.h")?, &file("commands.txt")?, &|name| {
        let base = name.rsplit('/').next().unwrap_or(name);
        file(&format!("assets/{base}"))
    })
}

/// The 360's rules, read the first time they are asked for.
pub fn xbox() -> Result<&'static Schema> {
    static RULES: OnceLock<std::result::Result<Schema, Error>> = OnceLock::new();
    RULES.get_or_init(|| load(&|name| definition(name).map(str::to_string))).as_ref().map_err(Clone::clone)
}
