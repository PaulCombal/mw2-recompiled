// The loading rules the converter reads zones by, read once: the 360 title update's (the
// zone crate's, zone/defs/), which it reads the game's files by, and the PC game's
// (zone/defs_pc/, embedded by build.rs), which it reads a PC map by.

use crate::error::{Error, Result};
use mw2_zone::schema::Schema;
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/pc_definitions.rs"));

pub fn xbox() -> Result<&'static Schema> {
    Ok(mw2_zone::rules::xbox()?)
}

pub fn pc() -> Result<&'static Schema> {
    static RULES: OnceLock<std::result::Result<Schema, mw2_zone::Error>> = OnceLock::new();
    let rules = RULES.get_or_init(|| {
        mw2_zone::rules::load(&|name| {
            PC_DEFINITIONS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, text)| text.to_string())
                .ok_or_else(|| mw2_zone::Error::new(format!("the launcher has no definition file pc/{name}")))
        })
    });
    rules.as_ref().map_err(|e| Error::new(e.0.clone()))
}
