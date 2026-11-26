// The shader build id (src/shader_build.rs's BUILD_ID): a SHA-256 of the sources that
// decide what a compiled shader is -- this crate's (the translator, the pipeline layout,
// a shader's library) and the launcher's compile pass with its reader of which shaders
// the game has (launcher/src/precompile.rs, zone/src/ and the definitions it reads,
// zone/defs/). The launcher's compile pass fills the pipeline cache for one of these,
// and a player's copy compiles again when it changes. The game and the launcher both
// take it from here.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

// The game's SHA-256 (install/).
use mw2_install::crypto;

fn watch(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());
}

// Every `.rs` under `folder`, by its path from the repository's root.
fn rust_files(root: &Path, folder: &str, inputs: &mut BTreeSet<String>) {
    watch(&root.join(folder));
    for entry in fs::read_dir(root.join(folder)).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().into_string().unwrap();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            rust_files(root, &format!("{folder}/{name}"), inputs);
        } else if kind.is_file() && name.ends_with(".rs") {
            inputs.insert(format!("{folder}/{name}"));
        }
    }
}

// Every `.h` and `.txt` under `folder`, by its path from the repository's root.
fn definition_files(root: &Path, folder: &str, inputs: &mut BTreeSet<String>) {
    watch(&root.join(folder));
    for entry in fs::read_dir(root.join(folder)).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().into_string().unwrap();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            definition_files(root, &format!("{folder}/{name}"), inputs);
        } else if kind.is_file() && (name.ends_with(".h") || name.ends_with(".txt")) {
            inputs.insert(format!("{folder}/{name}"));
        }
    }
}

fn shader_build(root: &Path) -> String {
    // The compile pass by name: its folder holds the rest of the launcher, and Cargo
    // watches a folder with everything under it, so watching it would rerun this script
    // (and rebuild every crate above this one) for any launcher file.
    let mut inputs: BTreeSet<String> = BTreeSet::from(["launcher/src/precompile.rs".to_string()]);
    // The reader, and the 360's definitions it embeds (zone/build.rs): its header, its
    // rules and each asset's (zone/defs_pc/, the PC game's, are the map converter's).
    rust_files(root, "zone/src", &mut inputs);
    definition_files(root, "zone/defs", &mut inputs);
    rust_files(root, "shaders/src", &mut inputs);
    let text: String = inputs
        .iter()
        .map(|input| {
            let path = root.join(input);
            watch(&path);
            let data = fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            format!("{input} {}\n", crypto::sha256_hex(&data))
        })
        .collect();
    crypto::sha256_hex(text.as_bytes())
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.parent().unwrap();
    println!("cargo:rustc-env=MW2_SHADER_BUILD={}", shader_build(root));
}
