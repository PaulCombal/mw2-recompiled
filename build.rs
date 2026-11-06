// Builds the game's C++ -- the renderer (runtime/gpu/) with what reaches it, and the
// guest code's fallback -- and hands rustc what the link needs.
//
// The C++ is compiled as CMake compiled it: clang, -O3, the x86-64 level of the
// rest, ThinLTO and the profile in pgo/. It goes in as object files (link.rsp), not
// an archive: a strong sub_X in an archive would not be taken out of it to replace
// the guest code's weak one, and the hook would silently not run. A file is compiled
// again only when it, a header it includes or its flags changed.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

// src/runtime/install/crypto.rs's SHA-256, which the game checks its executable with.
#[path = "src/runtime/install/crypto.rs"]
#[allow(dead_code)]
mod crypto;

fn run(command: &mut Command) {
    let status = command.status().unwrap_or_else(|e| panic!("{command:?}: {e}"));
    assert!(status.success(), "{command:?} failed");
}

fn rerun_if(variable: &str) -> Option<String> {
    println!("cargo:rerun-if-env-changed={variable}");
    env::var(variable).ok().filter(|value| !value.is_empty())
}

// An option given as MW2_<NAME>=ON/OFF in the environment, as CMake took it: off is
// OFF, 0, FALSE, NO, N, IGNORE, NOTFOUND or empty.
fn option(variable: &str, default: bool) -> bool {
    println!("cargo:rerun-if-env-changed={variable}");
    match env::var(variable) {
        Ok(value) => {
            !["OFF", "0", "FALSE", "NO", "N", "IGNORE", "NOTFOUND", ""].contains(&value.to_ascii_uppercase().as_str())
        }
        Err(_) => default,
    }
}

fn watch(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());
}

fn sha256_file(path: &Path) -> String {
    watch(path);
    let data = fs::read(path).unwrap_or_else(|e| panic!("{}: {e} -- cargo xtask makes it", path.display()));
    crypto::sha256_hex(&data)
}

// A target folder holds one title's guest code. The crates of another have the
// same path, generated/, and older files, so cargo would call them up to date
// and link the wrong title's.
fn check_target_folder(out: &Path, title: &str) {
    let profile = out.ancestors().nth(3).unwrap();
    let stamp = profile.join("mw2-title");
    match fs::read_to_string(&stamp) {
        Ok(built) if built != title => panic!(
            "{} holds the guest code of {built}, and generated/ is {title}: build in another folder (CARGO_TARGET_DIR)",
            profile.display()
        ),
        Ok(_) => {}
        Err(_) => fs::write(&stamp, title).unwrap(),
    }
}

// runtime/title.h as Rust. Its four blocks are, in order, the multiplayer's and
// the campaign's table for title update 6, then the same for the disc.
fn write_title(root: &Path, out: &Path, title: &str, version: &str) {
    let source = fs::read_to_string(root.join("runtime/title.h")).unwrap();
    let wanted = match (title, version) {
        ("mp", "tu6") => 0,
        ("sp", "tu6") => 1,
        ("mp", _) => 2,
        (_, _) => 3,
    };

    let mut block = None;
    let mut names = BTreeSet::new();
    let mut code = String::new();
    let mut symbols = String::new();
    for line in source.lines() {
        let mut words = line.split_whitespace();
        match (words.next(), words.next(), words.next()) {
            (Some("#if"), ..) if block.is_none() && line.contains("MW2_TITLE_MP") => block = Some(0),
            (Some("#elif" | "#else"), ..) => block = block.map(|n: i32| n + 1),
            (Some("#endif"), ..) if block.is_some() => break,
            (Some("#define"), Some(name), Some(value)) if block.is_some() && name.starts_with("T_") => {
                names.insert(name.to_string());
                if block != Some(wanted) {
                    continue;
                }
                println!("cargo:rustc-cfg={name}");
                if value.len() == 8 && value.chars().all(|c| c.is_ascii_digit() || ('A'..='F').contains(&c)) {
                    code += &format!(
                        "unsafe extern \"C\" {{\n\
                         #[link_name = \"sub_{value}\"] fn current_{name}(ctx: &mut PPCContext, base: *mut u8);\n\
                         #[link_name = \"__imp__sub_{value}\"] fn original_{name}(ctx: &mut PPCContext, base: *mut u8);\n\
                         }}\n\
                         pub const {name}: GuestFunction = GuestFunction::new(current_{name}, original_{name});\n"
                    );
                    symbols += &format!("({name}) => {{ \"sub_{value}\" }};\n");
                } else {
                    code += &format!("pub const {name}: u32 = {};\n", value.trim_end_matches('u'));
                }
            }
            _ => {}
        }
    }
    assert_eq!(block, Some(3), "runtime/title.h no longer has its four blocks");
    let title_name = match wanted {
        0 => "multiplayer",
        1 => "campaign",
        2 => "multiplayer, disc version",
        _ => "campaign, disc version",
    };
    code += &format!("pub const TITLE_NAME: &std::ffi::CStr = c\"{title_name}\";\n");
    // Every title's names: a #[cfg] of a name no table has is a typing mistake.
    for name in names {
        println!("cargo:rustc-check-cfg=cfg({name})");
    }
    code += &format!("macro_rules! hook_symbol {{\n{symbols}}}\n");
    fs::write(out.join("title.rs"), code).unwrap();
}

// What this build's shaders come out as (runtime/gpu/vulkan/shader_build.h): a
// SHA-256 of the sources that decide it, the translators' (the C++ one, which the
// launcher compiles, and the Rust one in src/runtime/gpu/shader with its forwarders,
// which the game runs) and the pipeline layout's, and the launcher's reader of which
// shaders the game has. The compile pass's cache
// is for one of these, and a player's copy compiles again when it changes. CMake
// works out the launcher's the same way (CMakeLists.txt), and the two must agree.
fn shader_build(root: &Path) -> String {
    type Pattern = (&'static str, fn(&str) -> bool);
    let patterns: [Pattern; 14] = [
        ("runtime/gpu/vulkan", |name| name.starts_with("shader_translator.")),
        ("runtime/gpu/vulkan", |name| name == "shader_translator_rust.cpp"),
        ("src/runtime/gpu/shader", |name| name.ends_with(".rs")),
        ("runtime/gpu/vulkan", |name| name.starts_with("spirv.")),
        ("runtime/gpu/vulkan", |name| name == "bindings.h"),
        ("runtime/gpu/vulkan", |name| name == "xenos_ucode.h"),
        ("runtime/gpu/vulkan", |name| name.starts_with("shader_build.")),
        ("runtime/gpu/vulkan", |name| name.starts_with("pipeline.")),
        ("launcher", |name| name.starts_with("precompile.")),
        ("launcher/zone", |name| name.ends_with(".cpp")),
        ("launcher/zone", |name| name.ends_with(".h")),
        ("launcher/zone/defs", |name| name.ends_with(".h")),
        ("launcher/zone/defs", |name| name.ends_with(".txt")),
        ("launcher/zone/defs/assets", |name| name.ends_with(".txt")),
    ];
    let mut inputs = BTreeSet::new();
    for (folder, matches) in patterns {
        watch(&root.join(folder));
        for entry in fs::read_dir(root.join(folder)).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            if matches(&name) {
                inputs.insert(format!("{folder}/{name}"));
            }
        }
    }
    let text: String = inputs.iter().map(|input| format!("{input} {}\n", sha256_file(&root.join(input)))).collect();
    crypto::sha256_hex(text.as_bytes())
}

// The profile of this executable, pgo/<title>.tar.xz unpacked under a name its hash
// makes, so that a new one compiles everything again with it. The disc's version
// (tu0) is other code, and has none.
fn profile(root: &Path, out: &Path, title: &str, version: &str) -> Option<PathBuf> {
    let archive = root.join(format!("pgo/{title}.tar.xz"));
    watch(&archive);
    if !option("MW2_PGO", true) || version != "tu6" || !archive.exists() {
        return None;
    }
    let folder = out.join(format!("pgo-{}", &sha256_file(&archive)[..16]));
    let profile = folder.join(format!("{title}.profdata"));
    if !profile.exists() {
        // Unpacked beside it and renamed, so that a stopped build leaves no half profile.
        let unpacking = out.join("pgo-unpacking");
        let _ = fs::remove_dir_all(&unpacking);
        fs::create_dir_all(&unpacking).unwrap();
        run(Command::new("tar").arg("-xJf").arg(&archive).arg("-C").arg(&unpacking));
        let _ = fs::remove_dir_all(&folder);
        fs::rename(&unpacking, &folder).unwrap();
    }
    Some(profile)
}

// The Vulkan a Windows build compiles and links against. The loader, vulkan-1.dll,
// comes with every GPU driver, so nothing is shipped: the headers are the build
// machine's (libvulkan-dev, or the folder MW2_VULKAN_HEADERS names), of the version
// the C++ build fetches (cmake/vulkan-windows.cmake), copied so that its other headers
// stay out of a Windows compile, and the import library is made from them -- every vk* entry point they
// declare, of which the renderer binds only the core and surface ones the loader
// exports.
fn windows_vulkan(root: &Path, out: &Path, mingw: &Path) -> (PathBuf, PathBuf) {
    let system = PathBuf::from(rerun_if("MW2_VULKAN_HEADERS").unwrap_or_else(|| "/usr/include".into()));
    let include = out.join("vulkan-headers");
    for folder in ["vulkan", "vk_video"] {
        let from = system.join(folder);
        assert!(from.is_dir(), "{} is missing: install the Vulkan headers (libvulkan-dev)", from.display());
        watch(&from);
        fs::create_dir_all(include.join(folder)).unwrap();
        for entry in fs::read_dir(&from).unwrap() {
            let entry = entry.unwrap();
            if !entry.path().is_file() {
                continue;
            }
            // Only what changed: a copy is newer than every object, which would all be
            // compiled again.
            let copy = include.join(folder).join(entry.file_name());
            let bytes = fs::read(entry.path()).unwrap();
            if fs::read(&copy).ok().as_ref() != Some(&bytes) {
                fs::write(&copy, bytes).unwrap();
            }
        }
    }
    let fetched = fs::read_to_string(root.join("cmake/vulkan-windows.cmake")).unwrap();
    let fetched = fetched.split("refs/tags/v").nth(1).and_then(|rest| rest.split(".tar").next()).unwrap().to_string();
    let core = fs::read_to_string(include.join("vulkan/vulkan_core.h")).unwrap();
    let patch = core.lines().find_map(|line| line.strip_prefix("#define VK_HEADER_VERSION ")).unwrap_or("none").trim();
    assert!(
        fetched.ends_with(&format!(".{patch}")),
        "{} has the Vulkan headers 1.x.{patch}, and the C++ build fetches {fetched}: MW2_VULKAN_HEADERS=<folder> names others",
        system.display()
    );
    let mut names = Vec::new();
    for header in ["vulkan_core.h", "vulkan_win32.h"] {
        for line in fs::read_to_string(include.join("vulkan").join(header)).unwrap().lines() {
            let Some(at) = line.find("VKAPI_CALL vk") else { continue };
            let name = &line[at + "VKAPI_CALL ".len()..];
            let Some(end) = name.find('(') else { continue };
            let name = &name[..end];
            if name.chars().all(|c| c.is_ascii_alphanumeric()) && !names.iter().any(|known| known == name) {
                names.push(name.to_string());
            }
        }
    }
    let definitions = out.join("vulkan-1.def");
    fs::write(&definitions, format!("LIBRARY vulkan-1.dll\nEXPORTS\n    {}\n", names.join("\n    "))).unwrap();
    let library = out.join("libvulkan-1.a");
    run(Command::new(mingw.join("bin/x86_64-w64-mingw32-dlltool"))
        .args(["-m", "i386:x86-64", "-d"])
        .arg(&definitions)
        .arg("-l")
        .arg(&library));
    (include, library)
}

// Whether the build machine has Vulkan's headers: without them the renderer is
// built with no window.
fn linux_vulkan(compiler: &Path, out: &Path) -> bool {
    let probe = out.join("vulkan-probe.cpp");
    fs::write(&probe, "#include <vulkan/vulkan.h>\n").unwrap();
    Command::new(compiler).arg("-fsyntax-only").arg(&probe).status().is_ok_and(|status| status.success())
}

// One C++ file and how it is compiled.
struct Unit {
    source: PathBuf,
    flags: Vec<String>,
}

// The files of a depfile (clang's -MD), the object's own name first.
fn dependencies(depfile: &Path) -> Option<Vec<PathBuf>> {
    let text = fs::read_to_string(depfile).ok()?;
    let text = text.replace("\\\n", " ");
    let (_, files) = text.split_once(": ")?;
    let mut found = Vec::new();
    let mut current = String::new();
    let mut characters = files.chars().peekable();
    while let Some(c) = characters.next() {
        match c {
            '\\' if characters.peek() == Some(&' ') => {
                current.push(' ');
                characters.next();
            }
            ' ' | '\n' | '\r' | '\t' => {
                if !current.is_empty() {
                    found.push(PathBuf::from(std::mem::take(&mut current)));
                }
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        found.push(PathBuf::from(current));
    }
    Some(found)
}

// The object of a unit compiled before with the same flags, when nothing it was
// made from has changed since.
fn up_to_date(folder: &Path, signature: &str) -> Option<PathBuf> {
    let stamp = fs::read_to_string(folder.join("stamp")).ok()?;
    let (made_with, object) = stamp.split_once('\n')?;
    if made_with != signature {
        return None;
    }
    let object = PathBuf::from(object);
    let made = fs::metadata(&object).ok()?.modified().ok()?;
    for input in dependencies(&folder.join("depfile"))? {
        if fs::metadata(&input).ok()?.modified().ok()? > made {
            return None;
        }
    }
    Some(object)
}

// Each unit's object, compiled again where needed, as many at a time as cargo
// gives jobs. clang is run as CMake ran it, with these flags and nothing from the
// environment, and what it says is passed on: an error in full, a warning as cargo's.
fn compile(units: &[Unit], compiler: &Path, root: &Path, objects: &Path) -> Vec<PathBuf> {
    let jobs = env::var("NUM_JOBS").ok().and_then(|jobs| jobs.parse().ok()).unwrap_or(1usize);
    let next = AtomicUsize::new(0);
    let results = Mutex::new(vec![None; units.len()]);
    let failed = Mutex::new(Vec::new());
    thread::scope(|scope| {
        for _ in 0..jobs.min(units.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(unit) = units.get(index) else { break };
                    // A folder of its own, named by the source's path under the repository.
                    let folder = objects.join(unit.source.strip_prefix(root).unwrap());
                    let signature =
                        format!("{} {} {}", compiler.display(), unit.source.display(), unit.flags.join(" "));
                    let object = up_to_date(&folder, &signature).or_else(|| {
                        fs::create_dir_all(&folder).unwrap();
                        let _ = fs::remove_file(folder.join("stamp"));
                        let object = folder.join("object.o");
                        let output = Command::new(compiler)
                            .args(&unit.flags)
                            .arg("-MD")
                            .arg("-MF")
                            .arg(folder.join("depfile"))
                            .arg("-c")
                            .arg(&unit.source)
                            .arg("-o")
                            .arg(&object)
                            .output()
                            .unwrap_or_else(|e| panic!("{}: {e}", compiler.display()));
                        let said = String::from_utf8_lossy(&output.stderr);
                        if !output.status.success() {
                            failed.lock().unwrap().push(format!("{}:\n{said}", unit.source.display()));
                            return None;
                        }
                        for line in said.lines() {
                            println!("cargo:warning={line}");
                        }
                        fs::write(folder.join("stamp"), format!("{signature}\n{}", object.display())).unwrap();
                        Some(object)
                    });
                    results.lock().unwrap()[index] = object;
                }
            });
        }
    });
    let failed = failed.into_inner().unwrap();
    assert!(failed.is_empty(), "the C++ did not compile:\n{}", failed.join("\n"));
    results.into_inner().unwrap().into_iter().map(Option::unwrap).collect()
}

fn sources(folder: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "cpp"))
        .collect();
    found.sort();
    found
}

// sdl3-sys builds SDL once in a target folder and never again: its build script asks
// cargo to watch nothing. What it was built with is written beside it the first time,
// and a build with other toolchain files or another llvm-mingw stops rather than link
// the old one. And it must be the SDL CMake fetches for the launcher.
fn check_sdl(root: &Path, sdl: &Path) {
    let mut inputs = String::new();
    for file in ["cmake/sdl.cmake", "cmake/sdl-mingw-w64.cmake", "cmake/mingw-w64.cmake"] {
        inputs += &format!("{file}\n{}\n", fs::read_to_string(root.join(file)).unwrap());
    }
    if env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows" {
        inputs += &format!("LLVM_MINGW={}\n", env::var("LLVM_MINGW").unwrap_or_default());
    }
    let stamp = sdl.join("mw2-built-with");
    match fs::read_to_string(&stamp) {
        Ok(built) if built != inputs => panic!(
            "{} was built with other toolchain files or another llvm-mingw: `cargo clean -p sdl3-sys` (with --target and \
             CARGO_TARGET_DIR as this build has them) builds it again",
            sdl.display()
        ),
        Ok(_) => {}
        Err(_) => fs::write(&stamp, inputs).unwrap(),
    }

    let header = fs::read_to_string(sdl.join("include/SDL3/SDL_version.h")).unwrap();
    let part = |name: &str| {
        let line = header.lines().find(|line| line.starts_with(&format!("#define SDL_{name}_VERSION"))).unwrap();
        line.split_whitespace().nth(2).unwrap().to_string()
    };
    let built = format!("{}.{}.{}", part("MAJOR"), part("MINOR"), part("MICRO"));
    let cmake = fs::read_to_string(root.join("CMakeLists.txt")).unwrap();
    let fetched = cmake.lines().find_map(|line| line.trim().strip_prefix("GIT_TAG release-")).unwrap_or("none");
    assert_eq!(built, fetched, "sdl3-sys builds SDL {built}, and CMakeLists.txt fetches {fetched} for the launcher");
}

// The renderer moves to Rust a file at a time (docs/building.md, "The renderer moves
// to Rust"). With the rust-renderer feature, the C++ files Rust has taken over are not
// compiled, each gives the Rust its cfg mw2_owns_<stem>, which turns on the Rust that
// stands in for it (its hooks, its exports, bridge.rs's routes), and the files that
// exist only to forward the C++ renderer's calls to Rust are compiled. Without the
// feature the C++ renderer is built whole, as it was.
const RUST_OWNS: &[&str] =
    &["runtime/gpu/texture.cpp", "runtime/gpu/vulkan/texture_formats.cpp", "runtime/gpu/vulkan/shader_translator.cpp"];
const RUST_ONLY: &[&str] = &["runtime/rust_renderer.cpp", "runtime/gpu/vulkan/shader_translator_rust.cpp"];

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let windows = env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows";
    let diagnostics = env::var_os("CARGO_FEATURE_DIAGNOSTICS").is_some();
    let portable = env::var_os("CARGO_FEATURE_PORTABLE").is_some();
    let rust_renderer = env::var_os("CARGO_FEATURE_RUST_RENDERER").is_some();

    // What cargo xtask last generated, as "<title> <version>".
    let stamp = root.join("generated.title");
    watch(&stamp);
    let generated = fs::read_to_string(&stamp).expect("generated.title is missing: run cargo xtask recompile first");
    let generated = generated.trim();
    let (title, version) = generated.split_once(' ').expect("generated.title is not \"<title> <version>\"");
    check_target_folder(&out, generated);

    write_title(&root, &out, title, version);
    println!("cargo:rustc-check-cfg=cfg(mw2_title_mp)");
    println!("cargo:rustc-check-cfg=cfg(mw2_version_tu0)");
    // Set by tools/kernel_diff.py alone, for its fixed clock and random bytes, and by
    // runtime_diff.py, whose screens have no XAM.
    println!("cargo:rustc-check-cfg=cfg(mw2_rig)");
    println!("cargo:rustc-check-cfg=cfg(mw2_screens_rig)");
    println!("cargo:rustc-check-cfg=cfg(mw2_xma_rig)");
    // tools/hook_diff.py's: a heap it can take back, and a game folder it names.
    println!("cargo:rustc-check-cfg=cfg(mw2_hook_rig)");
    if title == "mp" {
        println!("cargo:rustc-cfg=mw2_title_mp");
    }
    if version == "tu0" {
        println!("cargo:rustc-cfg=mw2_version_tu0");
    }
    // MW2_LOGGING=OFF compiles every log line to nothing, the C++'s and the Rust's
    // (src/log.rs).
    println!("cargo:rustc-check-cfg=cfg(mw2_no_logging)");
    let logging = option("MW2_LOGGING", true);
    if !logging {
        println!("cargo:rustc-cfg=mw2_no_logging");
    }

    let release = rerun_if("MW2_RELEASE_TAG").unwrap_or_default();
    println!("cargo:rustc-env=MW2_RELEASE_TAG={release}");

    // A game executable runs only the title executable it was recompiled from (src/runtime/install/).
    let xex = match (title, version) {
        ("mp", "tu6") => "mw2/default_mp.xex",
        ("sp", "tu6") => "mw2/default.xex",
        ("mp", _) => "mw2/tu0/default_mp.xex",
        (_, _) => "mw2/tu0/default.xex",
    };
    println!("cargo:rustc-env=MW2_XEX_SHA256={}", sha256_file(&root.join(xex)));

    // What Find Game offers where no relay hands it out (src/runtime/online/steam/): the
    // relay's own file, so a release has one list.
    let playlists = root.join("relay/playlists.info");
    watch(&playlists);
    println!("cargo:rustc-env=MW2_PLAYLISTS={}", playlists.display());

    // ---- the C++ ----------------------------------------------------------------
    let ppc = root.join(match (title, version) {
        ("sp", "tu6") => "ppc",
        ("mp", "tu6") => "ppc_mp",
        ("sp", _) => "ppc_tu0",
        (_, _) => "ppc_mp_tu0",
    });
    let mingw = windows
        .then(|| PathBuf::from(rerun_if("LLVM_MINGW").expect("a Windows build needs LLVM_MINGW=<llvm-mingw folder>")));
    let (compiler, archiver) = match &mingw {
        Some(mingw) => (mingw.join("bin/x86_64-w64-mingw32-clang++"), mingw.join("bin/llvm-ar")),
        None => (PathBuf::from("clang++-18"), PathBuf::from("llvm-ar-18")),
    };
    // The SDL sdl3-sys built, whose headers the renderer's C++ includes.
    let sdl = env::var("DEP_SDL3_OUT_DIR").expect("sdl3-sys named no SDL it built");
    check_sdl(&root, Path::new(&sdl));

    // How everything is optimised. ThinLTO optimises across files at the link, and
    // the profile, the counts of a recorded play (tools/pgo_record.sh), tells the
    // compiler which branches are taken and which calls are hot: together ~13% off the
    // CPU time of the thread that limits the frame rate (docs/building.md).
    // MW2_PGO_TRAINING=ON builds the counting executable instead.
    let training = option("MW2_PGO_TRAINING", false);
    let lto = option("MW2_LTO", true) && !training;
    let mut common: Vec<String> = ["-O3", "-DNDEBUG", "-std=gnu++20"].map(String::from).to_vec();
    let mut link: Vec<String> = vec!["-O3".into()];
    if training {
        common.push("-fprofile-instr-generate".into());
        link.push("-fprofile-instr-generate".into());
    } else {
        if lto {
            common.push("-flto=thin".into());
            // What the linker made of each module, for the next link to reuse what did
            // not change. A result taken from it is the one the linker would have made.
            let cache = rerun_if("MW2_LTO_CACHE")
                .or_else(|| rerun_if("MW2_BUILD_DIR").map(|build| format!("{build}/lto-cache")))
                .map_or_else(|| out.join("lto-cache"), PathBuf::from);
            link.push("-flto=thin".into());
            link.push(format!("-Wl,--thinlto-cache-dir={}", cache.display()));
        }
        if let Some(profile) = profile(&root, &out, title, version) {
            // A function changed since the recording is compiled without its counts.
            common.push(format!("-fprofile-instr-use={}", profile.display()));
            common.push("-Wno-profile-instr-unprofiled".into());
            common.push("-Wno-profile-instr-out-of-date".into());
        }
    }
    let mut definitions = vec!["MW2_TRACE_INDIRECT=1".to_string()];
    if title == "mp" {
        definitions.push("MW2_TITLE_MP=1".into());
    }
    if version == "tu0" {
        definitions.push("MW2_VERSION_TU0=1".into());
    }
    if !logging {
        definitions.push("MW2_NO_LOGGING=1".into());
    }
    let x86_level = "-march=x86-64-v2".to_string();
    let simde = root.join("XenonRecomp-rs/thirdparty/simde");

    // The renderer and its glue.
    let mut runtime: Vec<String> = common.clone();
    runtime.push(x86_level.clone());
    runtime.extend(definitions.iter().map(|definition| format!("-D{definition}")));
    runtime.extend(
        [format!("MW2_DIAGNOSTICS={}", u8::from(diagnostics)), "MW2_RUST=1".into(), "MW2_USE_SDL=1".into()]
            .map(|definition| format!("-D{definition}")),
    );
    if rust_renderer {
        runtime.push("-DMW2_RUST_RENDERER=1".into());
    }
    for include in [root.join("runtime"), PathBuf::from(&sdl).join("include"), ppc.clone(), simde.clone()] {
        runtime.push(format!("-I{}", include.display()));
    }
    let mut libraries = Vec::new();
    match &mingw {
        Some(mingw) => {
            let (include, library) = windows_vulkan(&root, &out, mingw);
            runtime.push("-DMW2_HAVE_VULKAN=1".into());
            runtime.push("-isystem".into());
            runtime.push(include.display().to_string());
            libraries.push(library.display().to_string());
        }
        None if linux_vulkan(&compiler, &out) => {
            runtime.push("-DMW2_HAVE_VULKAN=1".into());
            libraries.push("-lvulkan".into());
        }
        None if rust_renderer => {
            panic!("Vulkan's headers are missing: install libvulkan-dev, or build the C++ renderer (--renderer cpp)")
        }
        None => println!("cargo:warning=Vulkan's headers are missing: the game is built with no window"),
    }

    let mut units = Vec::new();
    let mut files = Vec::new();
    for folder in ["runtime/gpu", "runtime/gpu/vulkan"] {
        watch(&root.join(folder));
        files.extend(sources(&root.join(folder)));
    }
    for file in ["runtime/image_move.cpp", "runtime/shader_preload.cpp", "runtime/rust_calls.cpp"] {
        files.push(root.join(file));
    }
    // Every file of the two lists must exist: a misspelt one would leave a file in or
    // out without a word.
    for listed in RUST_OWNS.iter().chain(RUST_ONLY) {
        assert!(root.join(listed).is_file(), "build.rs names {listed}, which is not there");
    }
    // What Rust takes over must be a file this build compiles, whose stem names it alone.
    for owned in RUST_OWNS {
        assert!(files.contains(&root.join(owned)), "RUST_OWNS names {owned}, which build.rs does not compile");
        let named = |file: &&PathBuf| file.file_stem() == Path::new(owned).file_stem();
        assert!(files.iter().filter(named).count() == 1, "RUST_OWNS: another file has the stem of {owned}");
    }
    let stem = |path: &str| Path::new(path).file_stem().unwrap().to_str().unwrap().to_string();
    for file in &files {
        let relative = file.strip_prefix(&root).unwrap().to_str().unwrap();
        if !RUST_ONLY.contains(&relative) {
            println!("cargo:rustc-check-cfg=cfg(mw2_owns_{})", stem(relative));
        }
    }
    println!("cargo:rustc-check-cfg=cfg(mw2_gpu_rig)");
    let mut cfgs = vec![format!("renderer {}", if rust_renderer { "rust" } else { "cpp" })];
    if rust_renderer {
        files.retain(|file| !RUST_OWNS.contains(&file.strip_prefix(&root).unwrap().to_str().unwrap()));
        for owned in RUST_OWNS {
            println!("cargo:rustc-cfg=mw2_owns_{}", stem(owned));
            cfgs.push(format!("mw2_owns_{}", stem(owned)));
        }
        for only in RUST_ONLY {
            let only = root.join(only);
            if !files.contains(&only) {
                files.push(only);
            }
        }
    } else {
        files.retain(|file| !RUST_ONLY.contains(&file.strip_prefix(&root).unwrap().to_str().unwrap()));
    }
    // The kernel imports the runtime does not have (cargo xtask writes them).
    files.push(ppc.join("stubs_generated.cpp"));
    if training {
        // The runtime leaves with _exit, which writes no profile: this writes it first.
        files.push(root.join("runtime/pgo/training.cpp"));
        link.push("-Wl,--wrap=_exit".into());
    }
    let build_id = root.join("runtime/gpu/vulkan/shader_build.cpp");
    for source in files {
        watch(&source);
        let mut flags = runtime.clone();
        if source == build_id {
            flags.push(format!("-DMW2_SHADER_BUILD=\"{}\"", shader_build(&root)));
        }
        units.push(Unit { source, flags });
    }
    // The headers the C++ includes, the guest code's among them.
    watch(&root.join("runtime"));
    watch(&ppc);
    watch(&simde);
    if mingw.is_none() && Path::new("/usr/include/vulkan").is_dir() {
        watch(Path::new("/usr/include/vulkan"));
    }

    // The guest code's fallback, generated with it. Its warnings are machine output's
    // noise, and guest arithmetic wraps as the console's does. The console's hardware
    // registers are plain memory here: the stores the recompiler marks go through a hook
    // so that the XMA decoder sees its registers written (runtime/mmio_hook.h).
    let fallback = root.join(format!("{}_rs/cpp/ppc_fallback.cpp", ppc.display()));
    watch(&fallback);
    let mut flags = common.clone();
    flags.extend(["-w".into(), "-fwrapv".into(), x86_level]);
    flags.push("-include".into());
    flags.push(root.join("runtime/mmio_hook.h").display().to_string());
    flags.extend(definitions.iter().map(|definition| format!("-D{definition}")));
    flags.push(format!("-I{}", ppc.display()));
    flags.push(format!("-I{}", simde.display()));
    units.push(Unit { source: fallback, flags });

    let mut objects = compile(&units, &compiler, &root, &out.join("cpp"));
    // An archive, as the guest code's other half: only what is asked for is taken.
    let fallback_object = objects.pop().unwrap();
    let archive = out.join("libppc_fallback.a");
    let archived = fs::metadata(&archive).and_then(|archive| archive.modified()).ok();
    if archived.is_none_or(|archived| fs::metadata(&fallback_object).unwrap().modified().unwrap() > archived) {
        let _ = fs::remove_file(&archive);
        run(Command::new(&archiver).arg("rcsD").arg(&archive).arg(&fallback_object));
    }

    // ---- the link ------------------------------------------------------------------
    // rustc links with clang (.cargo/config.toml), which takes the link-time
    // optimisation's level from the link line, and rustc asks for -nodefaultlibs, so
    // the C++ library is named.
    // ash links vulkan-1 by name: the import library made above is in OUT_DIR.
    if windows && rust_renderer {
        println!("cargo:rustc-link-search=native={}", out.display());
    }
    if windows {
        // One executable with nothing beside it: the C++ runtime and the threads linked
        // in. The recompiled code nests deep on the host stack, and a thread Windows
        // starts gets the size the executable asks for: 16 MB, where Linux gives 8.
        link.extend(["-static".into(), "-Wl,--stack,16777216".into()]);
        // A player's build opens no console window (src/runtime/platform.rs).
        if !diagnostics {
            link.push("-mwindows".into());
        }
        // rustc says -no-pie, which means nothing to a PE.
        link.push("-Wno-unused-command-line-argument".into());
    } else {
        // lld, which reads LLVM bitcode, and takes its inputs in any order: rustc puts
        // its system libraries ahead of these.
        link.push("-fuse-ld=lld".into());
        // rustc links with partial RELRO (.cargo/config.toml): everything bound at start
        // all the same, so the GOT is read-only once the program runs.
        link.push("-Wl,-z,now".into());
        // The sub_XXXXXXXX guest functions exported so backtrace_symbols can name them,
        // and nothing else (runtime/exports.list).
        let exports = root.join("runtime/exports.list");
        watch(&exports);
        link.push(format!("-Wl,--dynamic-list={}", exports.display()));
    }
    link.extend(objects.iter().map(|object| object.display().to_string()));
    link.push(archive.display().to_string());
    link.extend(libraries);
    if windows {
        // By file name: rustc's -Bdynamic would find the DLLs' import libraries.
        link.extend(
            ["-l:libc++.a", "-l:libunwind.a", "-lws2_32", "-liphlpapi", "-lwinmm", "-lbcrypt"].map(String::from),
        );
    } else if portable {
        link.push("-l:libstdc++.a".into());
    } else {
        link.push("-lstdc++".into());
    }
    let response = out.join("link.rsp");
    fs::write(&response, link.join("\n") + "\n").unwrap();
    println!("cargo:rustc-link-arg-bins=@{}", response.display());
    // Beside link.rsp, for cargo xtask to copy into its build folder: what its hook
    // check reads, and the cfgs the rigs build the Rust with (tools/gpu_diff.py).
    fs::write(out.join("build.txt"), format!("{title} {version} diagnostics={}\n", u8::from(diagnostics))).unwrap();
    fs::write(out.join("renderer_cfgs.txt"), cfgs.join("\n") + "\n").unwrap();
}
