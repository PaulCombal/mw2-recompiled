// What the launcher's Rust is built with: what the install (src/setup.rs) knows a
// player's files by, read from mw2/ as the game's build.rs reads its own (see hashes
// below); the self-update's (src/update.rs) release; and on Windows, the link's icon and
// the Vulkan import library.
//
// The build folder is the one cargo xtask configured for this build (MW2_BUILD_DIR): the
// version and the logging are its CMake options, read from its cache, and on Windows it
// makes the Vulkan loader's import library.

use mw2_install::crypto;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn watch(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());
}

fn rerun_if(name: &str) -> Option<String> {
    println!("cargo:rerun-if-env-changed={name}");
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn run(command: &mut Command) {
    // Its output to cargo's log, not to the lines cargo reads instructions from.
    let status = command.stdout(Stdio::from(std::io::stderr())).status().unwrap_or_else(|e| panic!("{command:?}: {e}"));
    assert!(status.success(), "{command:?}: {status}");
}

// What the install knows a player's files by: the SHA-256 of the images the disc's
// executables decrypt to (mw2/tu0/*.pe) and of those title update 6 makes of them
// (mw2/*.pe), which cargo xtask prepare writes, and of the fastfiles the update adds
// (mw2/update/), for the USA/Europe disc the build is made from; the other discs' are
// written in src/setup.rs. A disc-version build -- MW2_VERSION tu0 in the CMake build
// folder, as cargo xtask build --tu0 configures it -- installs the disc as it is: the
// executables it installs are the disc's, and it has no update (cfg mw2_version_tu0).
fn hashes(root: &Path, build: Option<&Path>) {
    let version = match build {
        Some(build) => fs::read_to_string(build.join("CMakeCache.txt"))
            .ok()
            .and_then(|cache| cache.lines().find_map(|line| line.strip_prefix("MW2_VERSION:STRING=")).map(String::from)),
        None => rerun_if("MW2_VERSION"),
    }
    .unwrap_or_else(|| "tu6".into());
    let tu0 = version == "tu0";
    println!("cargo:rustc-check-cfg=cfg(mw2_version_tu0)");
    if tu0 {
        println!("cargo:rustc-cfg=mw2_version_tu0");
    }
    let installed = if tu0 { "mw2/tu0" } else { "mw2" };
    let mut files = vec![
        ("MW2_XEX_SHA256_SP", format!("{installed}/default.pe")),
        ("MW2_XEX_SHA256_MP", format!("{installed}/default_mp.pe")),
        ("MW2_DISC_SHA256_SP", "mw2/tu0/default.pe".into()),
        ("MW2_DISC_SHA256_MP", "mw2/tu0/default_mp.pe".into()),
    ];
    if !tu0 {
        files.push(("MW2_UPDATE_SHA256_PATCH_MP", "mw2/update/patch_mp.ff".into()));
        files.push(("MW2_UPDATE_SHA256_DLC1_UI_MP", "mw2/update/dlc1_ui_mp.ff".into()));
        files.push(("MW2_UPDATE_SHA256_DLC2_UI_MP", "mw2/update/dlc2_ui_mp.ff".into()));
    }
    for (name, file) in files {
        let path = root.join(file);
        watch(&path);
        let hash = match fs::read(&path) {
            Ok(data) => crypto::sha256_hex(&data),
            // A check with no build folder checks the Rust all the same; an install
            // that knows no file by an empty hash refuses every disc.
            Err(_) if build.is_none() => String::new(),
            Err(e) => panic!("{}: {e} -- cargo xtask prepare makes it from the ISO and the title update", path.display()),
        };
        println!("cargo:rustc-env={name}={hash}");
    }
}

// The log the compile pass writes (src/precompile.rs), as the C++ wrote it: none in a
// build whose CMake folder has MW2_LOGGING off, as CMakeLists.txt compiles the game's
// lines to nothing then (cfg mw2_no_logging).
fn logging(build: Option<&Path>) {
    println!("cargo:rustc-check-cfg=cfg(mw2_no_logging)");
    let Some(build) = build else { return };
    let cache = fs::read_to_string(build.join("CMakeCache.txt")).unwrap_or_default();
    let value = cache.lines().find_map(|line| line.strip_prefix("MW2_LOGGING:BOOL=")).unwrap_or("ON");
    // CMake's false constants, and a name ending in -NOTFOUND.
    let upper = value.to_ascii_uppercase();
    if ["OFF", "0", "FALSE", "NO", "N", "IGNORE", "NOTFOUND", ""].contains(&upper.as_str())
        || upper.ends_with("-NOTFOUND")
    {
        println!("cargo:rustc-cfg=mw2_no_logging");
    }
}

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).parent().unwrap().to_path_buf();
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let windows = env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows";
    let build = rerun_if("MW2_BUILD_DIR").map(PathBuf::from);
    hashes(&root, build.as_deref());
    logging(build.as_deref());
    // The release this build is, for the self-update (src/update.rs) to compare with the
    // newest: cargo xtask's --release-tag, which the release workflow gives. A build
    // without one does not update.
    let release = rerun_if("MW2_RELEASE_TAG").unwrap_or_default();
    println!("cargo:rustc-env=MW2_RELEASE_TAG={release}");
    // Without a build folder nothing more is needed but on Windows, where the link takes
    // the Vulkan import library from it: a cargo check of the workspace (an editor's, say)
    // still checks the Rust.
    let Some(build) = build else { return };
    if !windows {
        return;
    }
    let mingw = PathBuf::from(rerun_if("LLVM_MINGW").expect("a Windows build needs LLVM_MINGW=<llvm-mingw folder>"));
    // The Vulkan loader's import library, which CMake makes in the build folder
    // (cmake/vulkan-windows.cmake): vulkan-1.dll comes with every GPU driver, so nothing is
    // shipped.
    let cache = build.join("CMakeCache.txt");
    watch(&cache);
    let mut cmake = Command::new("cmake");
    cmake.arg("--build").arg(&build).args(["--target", "vulkan_import_library"]);
    run(&mut cmake);
    let vulkan = build.join("libvulkan-1.a");
    watch(&vulkan);
    // ash links vulkan-1 by name: the import library's folder.
    println!("cargo:rustc-link-search=native={}", build.display());
    // One executable with nothing beside it. rustc says -no-pie, which means nothing to a
    // PE.
    let mut link: Vec<String> = vec!["-static".into(), "-Wno-unused-command-line-argument".into()];
    // The program's icon, as Explorer and the taskbar show it: compiled as the game's is,
    // by the toolchain's windres.
    let icon = root.join("assets/icons/launcher.rc");
    watch(&icon);
    watch(&root.join("assets/icons/launcher.ico"));
    let resource = out.join("launcher.rc.res");
    run(Command::new(mingw.join("bin/x86_64-w64-mingw32-windres")).args(["-O", "coff"]).arg(&icon).arg(&resource));
    link.push(resource.display().to_string());
    let response = out.join("link.rsp");
    fs::write(&response, link.join("\n") + "\n").unwrap();
    println!("cargo:rustc-link-arg-bins=@{}", response.display());
}
