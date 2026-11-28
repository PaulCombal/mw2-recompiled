// Builds the game's C++ -- the GPU capture and the Rust's calls into it, diagnostics only:
// a player's build has none -- and hands rustc what the link needs.
//
// The C++ is compiled with clang, -O3, the x86-64 level of the rest and ThinLTO, and
// goes in as object files (link.rsp). A file is compiled again only when it, a header it
// includes or its flags changed.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

// The SHA-256 the game checks its executable with (install/).
use mw2_install::crypto;

fn run(command: &mut Command) {
    let status = command.status().unwrap_or_else(|e| panic!("{command:?}: {e}"));
    assert!(status.success(), "{command:?} failed");
}

fn rerun_if(variable: &str) -> Option<String> {
    println!("cargo:rerun-if-env-changed={variable}");
    env::var(variable).ok().filter(|value| !value.is_empty())
}

// An option given as MW2_<NAME>=ON/OFF in the environment: off is OFF, 0, FALSE, NO, N,
// IGNORE, NOTFOUND or empty, in any case.
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

// Whether the build machine has the loader's development library (libvulkan.so, from
// libvulkan-dev), which the renderer links by name with -lvulkan: an empty program linked
// against it. The game includes no Vulkan header on Linux, so the headers are not asked
// for.
fn linux_vulkan(compiler: &Path, out: &Path) -> bool {
    let probe = out.join("vulkan-probe.c");
    fs::write(&probe, "int main(void) { return 0; }\n").unwrap();
    Command::new(compiler)
        .args(["-x", "c"])
        .arg(&probe)
        .arg("-o")
        .arg(out.join("vulkan-probe"))
        .arg("-lvulkan")
        .status()
        .is_ok_and(|status| status.success())
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

// Each unit's object, compiled again where needed, as many at a time as cargo gives
// jobs. clang is run with these flags and nothing from the environment, and what it
// says is passed on: an error in full, a warning as cargo's.
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

// sdl3-sys builds SDL once in a target folder and never again: its build script asks
// cargo to watch nothing. What it was built with is written beside it the first time,
// and a build with other toolchain files or another llvm-mingw stops rather than link
// the old one.
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
}

// The C++ the game compiles with diagnostics, in its link order: the GPU capture and the
// calls the Rust makes of it (rust_calls.cpp).
const DIAGNOSTICS_ONLY: &[&str] = &["runtime/gpu/stream_capture.cpp", "runtime/rust_calls.cpp"];

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let windows = env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows";
    let diagnostics = env::var_os("CARGO_FEATURE_DIAGNOSTICS").is_some();

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

    // A game executable runs only the image it was recompiled from (src/runtime/install/),
    // which every region's executable shares.
    let image = match (title, version) {
        ("mp", "tu6") => "mw2/default_mp.pe",
        ("sp", "tu6") => "mw2/default.pe",
        ("mp", _) => "mw2/tu0/default_mp.pe",
        (_, _) => "mw2/tu0/default.pe",
    };
    println!("cargo:rustc-env=MW2_IMAGE_SHA256={}", sha256_file(&root.join(image)));

    // What Find Game offers where no relay hands it out (src/runtime/online/steam/): the
    // relay's own file, so a release has one list.
    let playlists = root.join("relay/playlists.info");
    watch(&playlists);
    println!("cargo:rustc-env=MW2_PLAYLISTS={}", playlists.display());

    // ---- the C++ ----------------------------------------------------------------
    let mingw = windows
        .then(|| PathBuf::from(rerun_if("LLVM_MINGW").expect("a Windows build needs LLVM_MINGW=<llvm-mingw folder>")));
    let compiler = match &mingw {
        Some(mingw) => mingw.join("bin/x86_64-w64-mingw32-clang++"),
        None => PathBuf::from("clang++-18"),
    };
    // The SDL sdl3-sys built, which the presenter links.
    let sdl = env::var("DEP_SDL3_OUT_DIR").expect("sdl3-sys named no SDL it built");
    check_sdl(&root, Path::new(&sdl));

    // How the C++ is optimised: ThinLTO, across files at the link.
    let lto = option("MW2_LTO", true);
    let mut common: Vec<String> = ["-O3", "-DNDEBUG", "-std=gnu++20"].map(String::from).to_vec();
    let mut link: Vec<String> = vec!["-O3".into()];
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
    let mut definitions: Vec<String> = Vec::new();
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

    let mut runtime: Vec<String> = common;
    runtime.push(x86_level.clone());
    runtime.extend(definitions.iter().map(|definition| format!("-D{definition}")));
    runtime.push(format!("-DMW2_DIAGNOSTICS={}", u8::from(diagnostics)));
    for include in [root.join("runtime"), PathBuf::from(&sdl).join("include")] {
        runtime.push(format!("-I{}", include.display()));
    }
    // The Windows import library is the shaders crate's (shaders/build.rs).
    let mut libraries = Vec::new();
    if mingw.is_none() {
        assert!(
            linux_vulkan(&compiler, &out),
            "Vulkan's loader library (libvulkan.so) is missing: install libvulkan-dev"
        );
        libraries.push("-lvulkan".to_string());
    }

    let mut units = Vec::new();
    // The GPU capture is diagnostics alone: a release links no trace of it, not even the
    // empty object's file symbol.
    let mut files: Vec<PathBuf> = Vec::new();
    if diagnostics {
        files.extend(DIAGNOSTICS_ONLY.iter().map(|file| root.join(file)));
    }
    for source in files {
        watch(&source);
        units.push(Unit { source, flags: runtime.clone() });
    }
    // The headers the C++ includes.
    watch(&root.join("runtime"));
    let objects = compile(&units, &compiler, &root, &out.join("cpp"));

    // The kernel imports the runtime does not have, a logging stub each, which cargo
    // xtask writes beside the recompiled Rust (src/runtime/kernel/stubs.rs). The guest
    // code has no C++: xtask's --no-cpp-fallback refuses a function the backend would
    // leave to it.
    let stubs = root.join("generated/kernel_stubs.rs");
    assert!(stubs.exists(), "{} is missing: run cargo xtask recompile (--keep-generated is enough)", stubs.display());
    println!("cargo:rustc-env=MW2_KERNEL_STUBS={}", stubs.display());

    // ---- the link ------------------------------------------------------------------
    // rustc links with clang (.cargo/config.toml), which takes the link-time
    // optimisation's level from the link line, and rustc asks for -nodefaultlibs, so
    // the C++ library is named.
    if windows {
        // One executable with nothing beside it: the C++ library and the threads linked
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
    if let Some(mingw) = &mingw {
        // The program's icon, as Explorer and the taskbar show it: the title's, compiled
        // by the toolchain's windres as the launcher's is (tools/icons.py makes it).
        let script = root.join(format!("assets/icons/{title}.rc"));
        watch(&script);
        watch(&root.join(format!("assets/icons/{title}.ico")));
        let resource = out.join("icon.res");
        run(Command::new(mingw.join("bin/x86_64-w64-mingw32-windres"))
            .args(["-O", "coff"])
            .arg(&script)
            .arg(&resource));
        link.push(resource.display().to_string());
    }
    link.extend(objects.iter().map(|object| object.display().to_string()));
    link.extend(libraries);
    if windows {
        // By file name: rustc's -Bdynamic would find the DLLs' import libraries.
        link.extend(
            ["-l:libc++.a", "-l:libunwind.a", "-lws2_32", "-liphlpapi", "-lwinmm", "-lbcrypt"].map(String::from),
        );
    } else if !objects.is_empty() {
        // Only a diagnostics build has C++ to serve.
        link.push("-lstdc++".into());
    }
    let response = out.join("link.rsp");
    fs::write(&response, link.join("\n") + "\n").unwrap();
    println!("cargo:rustc-link-arg-bins=@{}", response.display());
    // Beside link.rsp, for cargo xtask to copy into its build folder: what its hook
    // check reads.
    fs::write(out.join("build.txt"), format!("{title} {version} diagnostics={}\n", u8::from(diagnostics))).unwrap();
}
