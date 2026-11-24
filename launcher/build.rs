// The launcher's C++, launcher_core (CMakeLists.txt), built through CMake and linked
// with what CMake's link of it would take.
//
// The CMake build folder is the one cargo xtask configured for this build (MW2_BUILD_DIR):
// the title, the version, the release tag, the toolchain and the fetched sources are its
// options there, and the folder also holds the tools. This builds the library and the
// archives it needs in it, then reads what to link from launcher_core.txt, which CMake
// writes there: the archives, SDL's system libraries, the compiler's standard ones, the
// Vulkan import library on Windows, and the link's options (ThinLTO and its cache).
//
// rustc links with clang (.cargo/config.toml) and asks for -nodefaultlibs, so the C++
// library is named. On Linux the archives and, for a portable build, the static C++
// library and unwinder go in as rustc's own native libraries, which it puts ahead of the
// standard library's -lgcc_s: from a response file after them, lld would take the shared
// libgcc_s's symbols for libstdc++.a's, and the executable would need it. On Windows
// everything is static (-static) and goes in a response file, the icon too.
//
// It also gives the install (src/setup.rs) what it knows a player's files by, read from
// mw2/ as the game's build.rs reads its own: see hashes below; and the self-update
// (src/update.rs) the release the build is.

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

// The system libraries a CMake target asks for: "name" or "$<LINK_ONLY:name>" each; SDL's
// header target is no library. Each once, in the order first asked for.
fn system_libraries(lines: &[&str]) -> Vec<String> {
    let mut libraries: Vec<String> = Vec::new();
    for item in lines.iter().flat_map(|line| line.split_whitespace()) {
        let name = match item.strip_prefix("$<LINK_ONLY:").and_then(|rest| rest.strip_suffix('>')) {
            Some(name) => name,
            None if item.starts_with("$<TARGET_NAME:") => continue,
            None if !item.contains(['$', '<', '>', '/', ':']) => item,
            None => panic!("launcher_core.txt: a library links {item}, which this build does not know how to"),
        };
        if !libraries.iter().any(|known| known == name) {
            libraries.push(name.to_string());
        }
    }
    libraries
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

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).parent().unwrap().to_path_buf();
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let windows = env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows";
    let portable = env::var_os("CARGO_FEATURE_PORTABLE").is_some();
    let build = rerun_if("MW2_BUILD_DIR").map(PathBuf::from);
    hashes(&root, build.as_deref());
    // The release this build is, for the self-update (src/update.rs) to compare with the
    // newest: cargo xtask's --release-tag, which the release workflow gives. A build
    // without one does not update.
    let release = rerun_if("MW2_RELEASE_TAG").unwrap_or_default();
    println!("cargo:rustc-env=MW2_RELEASE_TAG={release}");
    // Without a build folder there is nothing to link: a cargo check of the workspace
    // (an editor's, say) still checks the Rust; a cargo build fails at the link, on
    // launcher_run.
    let Some(build) = build else {
        println!(
            "cargo:warning=MW2_BUILD_DIR is not set, so the launcher's C++ is not built or linked: \
             cargo xtask build configures the CMake build folder and sets it"
        );
        return;
    };

    // The C++ and what it is made from (the sources ninja's dependencies name, but the
    // fetched libraries' in the build folders). A change anywhere in these runs this
    // again, and CMake rebuilds what changed. cargo xtask has CMake build these targets
    // first, so ninja's own dependencies decide there, and a rebuilt archive runs this
    // again.
    for path in ["launcher", "runtime", "assets"] {
        watch(&root.join(path));
    }
    for path in ["cmake", "CMakeLists.txt"] {
        watch(&root.join(path));
    }
    watch(&build.join("CMakeCache.txt"));
    let mut targets = vec!["launcher_core", "SDL3-static", "zlibstatic"];
    if windows {
        targets.push("vulkan_import_library");
    }
    let mut cmake = Command::new("cmake");
    cmake.arg("--build").arg(&build).arg("--target").args(&targets);
    if let Ok(jobs) = env::var("NUM_JOBS") {
        cmake.args(["-j", &jobs]);
    }
    run(&mut cmake);

    let listing = build.join("launcher_core.txt");
    watch(&listing);
    let text = fs::read_to_string(&listing).unwrap_or_else(|e| panic!("{}: {e}", listing.display()));
    let field = |name: &str| -> Vec<&str> {
        text.lines().filter_map(|line| line.strip_prefix(name).and_then(|rest| rest.strip_prefix(' '))).collect()
    };
    let archives: Vec<PathBuf> = field("archive").into_iter().map(PathBuf::from).collect();
    // The launcher's own: the threads, on Linux (urlmon, the download's on Windows, is
    // src/download.rs's to name); then its libraries'.
    let mut uses = if windows { vec![] } else { vec!["pthread"] };
    uses.extend(field("uses"));
    let system = system_libraries(&uses);
    let standard: Vec<String> =
        field("standard").first().copied().unwrap_or_default().split_whitespace().map(String::from).collect();
    let lto: Vec<&str> = field("lto").first().copied().unwrap_or_default().splitn(2, ' ').collect();
    for archive in &archives {
        watch(archive);
    }

    // The link's options, as CMakeLists.txt gives them (add_link_options), and its -O3.
    let mut link: Vec<String> = vec!["-O3".into()];
    if lto.first() == Some(&"ON") {
        link.push("-flto=thin".into());
        link.push(format!("-Wl,--thinlto-cache-dir={}", lto.get(1).copied().unwrap_or_default()));
        if !windows {
            link.push("-fuse-ld=lld".into());
        }
    }

    if windows {
        let mingw =
            PathBuf::from(rerun_if("LLVM_MINGW").expect("a Windows build needs LLVM_MINGW=<llvm-mingw folder>"));
        // One executable with nothing beside it. rustc says -no-pie, which means nothing
        // to a PE.
        link.push("-static".into());
        link.push("-Wno-unused-command-line-argument".into());
        // The program's icon, as Explorer and the taskbar show it: compiled as CMake's
        // RC rule compiles it, by the toolchain's windres.
        let icon = root.join("assets/icons/launcher.rc");
        let resource = out.join("launcher.rc.res");
        run(Command::new(mingw.join("bin/x86_64-w64-mingw32-windres")).args(["-O", "coff"]).arg(&icon).arg(&resource));
        link.push(resource.display().to_string());
        link.extend(archives.iter().map(|archive| archive.display().to_string()));
        // The system libraries, the loader's import library, the compiler's standard ones.
        link.extend(system.iter().map(|library| format!("-l{library}")));
        let vulkan = PathBuf::from(field("vulkan").first().copied().expect("launcher_core.txt: no Vulkan library"));
        watch(&vulkan);
        // ash links vulkan-1 by name: the import library's folder.
        println!("cargo:rustc-link-search=native={}", vulkan.parent().unwrap().display());
        link.push(vulkan.display().to_string());
        link.extend(standard);
        // By file name: rustc's -Bdynamic would find the DLLs' import libraries.
        link.extend(["-l:libc++.a", "-l:libunwind.a"].map(String::from));
    } else {
        for archive in &archives {
            let name = archive.file_name().unwrap().to_str().unwrap();
            let name = name.strip_prefix("lib").and_then(|name| name.strip_suffix(".a")).unwrap();
            println!("cargo:rustc-link-search=native={}", archive.parent().unwrap().display());
            println!("cargo:rustc-link-lib=static:-bundle={name}");
        }
        if portable {
            // What -static-libstdc++ -static-libgcc give clang.
            for library in ["libstdc++.a", "libgcc_eh.a", "libgcc.a"] {
                println!("cargo:rustc-link-lib=static:-bundle,+verbatim={library}");
            }
        } else {
            println!("cargo:rustc-link-lib=dylib=stdc++");
        }
        // The loader is ash's (-lvulkan); then the system libraries.
        link.extend(system.iter().map(|library| format!("-l{library}")));
        link.extend(standard);
    }
    let response = out.join("link.rsp");
    fs::write(&response, link.join("\n") + "\n").unwrap();
    println!("cargo:rustc-link-arg-bins=@{}", response.display());
}
