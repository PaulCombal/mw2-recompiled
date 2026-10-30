//! protocol.h, which the game includes, must be what src/protocol.rs says.

use std::path::Path;

#[test]
fn protocol_header_is_current() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let config = cbindgen::Config::from_file(root.join("cbindgen.toml")).expect("cbindgen.toml");
    let bindings = cbindgen::Builder::new()
        .with_config(config)
        .with_src(root.join("src/protocol.rs"))
        .generate()
        .expect("cbindgen");
    let mut made = Vec::new();
    bindings.write(&mut made);
    let path = root.join("protocol.h");
    if std::env::var_os("BLESS").is_some() {
        std::fs::write(&path, &made).expect("protocol.h");
    }
    let held = std::fs::read(&path).expect("protocol.h");
    assert!(held == made, "protocol.h is out of date: BLESS=1 cargo test");
}
