// The build, from the retail disc to the executables:
//
//   cargo xtask prepare   [options]   XenonRecomp built, the executables and the game
//                                     data taken from the disc, the title update
//                                     applied, the flat image written
//   cargo xtask recompile [options]   and the guest code recompiled to Rust
//   cargo xtask build     [options]   and the game, the launcher and the tools built
//
// Each step does only what is not done yet, but the recompilation, which runs every
// time unless --keep-generated. Run from the repository's root (cargo finds the alias
// in .cargo/config.toml there). docs/building.md has the options' whole story.

mod check_hooks;
mod disc;
mod generate;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

type Result<T = ()> = std::result::Result<T, String>;

const USAGE: &str = "usage: cargo xtask <prepare|recompile|build> [options]

  --title sp|mp          sp (default): default.xex, the campaign and special ops;
                         mp: default_mp.xex, the multiplayer
  --tu0                  the disc's own executables instead of title update 6's
  --release              without diagnostics, and on Linux the launcher's C++ library linked in
  --windows              cross-compile for Windows with llvm-mingw
  --llvm-mingw <dir>     llvm-mingw's folder (or LLVM_MINGW in the environment)
  --build-dir <dir>      build there instead of build, build-mp, build-release, ...
  --targets <names>      build only these (mw2, launcher, ...), space-separated
  --release-tag <tag>    the release the build is published as (v0.3.0)
  --keep-generated       keep the recompiled tree already there
  --iso <path>           the retail disc image
  --tu <path>            title update 6's package, or a folder with its files
  --cmake-extra <arg>    passed to CMake's configure step; may be repeated

The game's options in a cargo build -- MW2_LOGGING, MW2_LTO, MW2_LTO_CACHE -- are taken
from the environment; MW2_LTO_CACHE is the launcher's too.";

const ISO: &str = "/home/paul/Téléchargements/Call of Duty - Modern Warfare 2 (USA, Europe).iso";
// The disc's executables, which the update patches, and the update's files.
const DISC: &str = "mw2/tu0";
const UPDATE: &str = "mw2/update";
// Where XenonRecomp and XenonAnalyse are built.
const XENON_BUILD: &str = "out/xenonrecomp";

#[derive(PartialEq)]
enum Step {
    Prepare,
    Recompile,
    Build,
}

struct Options {
    step: Step,
    title: String,
    tu0: bool,
    release: bool,
    windows: bool,
    llvm_mingw: Option<String>,
    build_dir: Option<String>,
    targets: Vec<String>,
    release_tag: String,
    keep_generated: bool,
    iso: String,
    tu: Option<String>,
    cmake_extra: Vec<String>,
}

fn parse() -> Result<Options> {
    let mut args = std::env::args().skip(1);
    let step = match args.next().as_deref() {
        Some("prepare") => Step::Prepare,
        Some("recompile") => Step::Recompile,
        Some("build") => Step::Build,
        _ => return Err(USAGE.into()),
    };
    let mut options = Options {
        step,
        title: "sp".into(),
        tu0: false,
        release: false,
        windows: false,
        llvm_mingw: std::env::var("LLVM_MINGW").ok().filter(|dir| !dir.is_empty()),
        build_dir: None,
        targets: Vec::new(),
        release_tag: String::new(),
        keep_generated: false,
        iso: ISO.into(),
        tu: None,
        cmake_extra: Vec::new(),
    };
    while let Some(arg) = args.next() {
        // --name value or --name=value.
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name.to_string(), Some(value.to_string())),
            _ => (arg.clone(), None),
        };
        let mut value = || inline.clone().or_else(|| args.next()).ok_or(format!("{name} needs a value\n\n{USAGE}"));
        match name.as_str() {
            "--title" => options.title = value()?,
            "--tu0" => options.tu0 = true,
            "--release" => options.release = true,
            "--windows" => options.windows = true,
            "--llvm-mingw" => options.llvm_mingw = Some(value()?),
            "--build-dir" => options.build_dir = Some(value()?),
            "--targets" => options.targets.extend(value()?.split_whitespace().map(String::from)),
            "--release-tag" => options.release_tag = value()?,
            "--keep-generated" => options.keep_generated = true,
            "--iso" => options.iso = value()?,
            "--tu" => options.tu = Some(value()?),
            "--cmake-extra" => options.cmake_extra.push(value()?),
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ => return Err(format!("unknown option {arg}\n\n{USAGE}")),
        }
        if inline.is_some() && matches!(name.as_str(), "--tu0" | "--release" | "--windows" | "--keep-generated") {
            return Err(format!("{name} takes no value"));
        }
    }
    if options.title != "sp" && options.title != "mp" {
        return Err(format!("--title must be sp or mp, not '{}'", options.title));
    }
    // The game's options are build.rs's, from the environment: in CMake's
    // arguments they would reach only the launcher and the tools.
    const OPTIONS: [&str; 3] = ["LOGGING", "LTO", "LTO_CACHE"];
    if options.cmake_extra.iter().any(|arg| {
        OPTIONS.iter().any(|option| {
            let name = format!("-DMW2_{option}");
            arg.strip_prefix(&name).is_some_and(|rest| rest.starts_with([':', '=']))
        })
    }) {
        return Err("a cargo build takes MW2_LOGGING, MW2_LTO and MW2_LTO_CACHE from the environment, \
                    not --cmake-extra"
            .into());
    }
    Ok(options)
}

// One title and version's files, as tools/title.py names them.
struct Title {
    name: String,
    version: &'static str,
    // Where the executable and its flat image are: mw2/, or mw2/tu0/ for the disc's.
    folder: &'static str,
    xex: &'static str,
    pe: &'static str,
    toml: String,
    tables: String,
    ppc: String,
}

impl Title {
    fn new(options: &Options) -> Title {
        let mp = options.title == "mp";
        let (toml, tables, ppc) = if mp {
            ("MW2MP.toml", "config/mw2mp_switch_tables.toml", "ppc_mp")
        } else {
            ("MW2.toml", "config/mw2_switch_tables.toml", "ppc")
        };
        let (version, folder, toml, tables, ppc) = if options.tu0 {
            (
                "tu0",
                DISC,
                toml.replace(".toml", ".tu0.toml"),
                tables.replace(".toml", ".tu0.toml"),
                format!("{ppc}_tu0"),
            )
        } else {
            ("tu6", "mw2", toml.into(), tables.into(), ppc.into())
        };
        Title {
            name: options.title.clone(),
            version,
            folder,
            xex: if mp { "default_mp.xex" } else { "default.xex" },
            pe: if mp { "default_mp.pe" } else { "default.pe" },
            toml,
            tables,
            ppc,
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn jobs() -> String {
    std::thread::available_parallelism().map_or(1, |jobs| jobs.get()).to_string()
}

// A command, its program named in a failure.
fn run(command: &mut Command) -> Result {
    let status = command.status().map_err(|e| format!("{}: {e}", command.get_program().to_string_lossy()))?;
    if !status.success() {
        return Err(format!("{} failed ({status})", command.get_program().to_string_lossy()));
    }
    Ok(())
}

// cargo build, with the game's build-script run picked out of cargo's messages: its
// OUT_DIR, whether the run was this time's or an earlier one's.
fn cargo_build(command: &mut Command) -> Result<PathBuf> {
    use std::io::BufRead;
    let mut child = command
        .arg("--message-format=json-render-diagnostics")
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cargo: {e}"))?;
    let mut out_dir = None;
    for line in std::io::BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line.map_err(|e| format!("cargo: {e}"))?;
        if !line.starts_with('{') {
            println!("{line}");
        } else if line.contains("\"reason\":\"build-script-executed\"") && line.contains("#mw2@") {
            let at = line.find("\"out_dir\":\"").ok_or("cargo named no out_dir")? + "\"out_dir\":\"".len();
            let end = line[at..].find('"').ok_or("cargo's out_dir has no end")?;
            out_dir = Some(PathBuf::from(&line[at..at + end]));
        }
    }
    let status = child.wait().map_err(|e| format!("cargo: {e}"))?;
    if !status.success() {
        return Err(format!("cargo failed ({status})"));
    }
    out_dir.ok_or_else(|| "cargo ran no build script for the game".into())
}

fn quiet(command: &mut Command) -> Result {
    run(command.stdout(Stdio::null()))
}

fn output(command: &mut Command) -> Result<String> {
    let output = command.stderr(Stdio::inherit()).output();
    let output = output.map_err(|e| format!("{}: {e}", command.get_program().to_string_lossy()))?;
    if !output.status.success() {
        return Err(format!("{} failed ({})", command.get_program().to_string_lossy(), output.status));
    }
    String::from_utf8(output.stdout).map_err(|_| format!("{} wrote other than UTF-8", command.get_program().display()))
}

// XenonRecomp's output, which it is not up to the build to hold to UTF-8, written to
// `log_path` whether or not it succeeded.
fn recompile_log(command: &mut Command, log_path: &str) -> Result<String> {
    let output = command.stderr(Stdio::inherit()).output();
    let output = output.map_err(|e| format!("{}: {e}", command.get_program().to_string_lossy()))?;
    let said = String::from_utf8_lossy(&output.stdout).into_owned();
    fs::write(log_path, &said).map_err(|e| format!("{log_path}: {e}"))?;
    if !output.status.success() {
        // What it said on the way out, the functions --no-cpp-fallback refuses among it,
        // without the progress lines; the whole of it is in the log.
        let reasons: Vec<&str> = said.lines().filter(|line| !line.starts_with("Recompiling functions...")).collect();
        return Err(format!(
            "{} failed ({}), its output in {log_path}:\n{}",
            command.get_program().to_string_lossy(),
            output.status,
            reasons[reasons.len().saturating_sub(100)..].join("\n")
        ));
    }
    Ok(said)
}

fn git(args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command.args(args);
    command
}

fn count_files(folder: &str) -> usize {
    fs::read_dir(folder).map_or(0, |entries| entries.count())
}

// The names in a folder, as a shell's glob lists them: sorted, without the hidden.
fn listing(folder: &Path) -> Result<Vec<String>> {
    let entries = fs::read_dir(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    Ok(names)
}

fn write_if_changed(path: &str, contents: &str) -> Result {
    if fs::read_to_string(path).ok().as_deref() != Some(contents) {
        fs::write(path, contents).map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(())
}

// XenonRecomp-rs is a submodule: fetched with its own on a first build, and at the
// commit the tree records, or a later one of its own history.
fn xenon_checkout() -> Result {
    let checked_out = Path::new("XenonRecomp-rs/CMakeLists.txt").exists()
        && fs::read_dir("XenonRecomp-rs/thirdparty").is_ok_and(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().is_dir())
                .all(|entry| fs::read_dir(entry.path()).is_ok_and(|mut files| files.next().is_some()))
        });
    if !checked_out {
        println!("    fetching XenonRecomp-rs and its submodules");
        run(&mut git(&["submodule", "update", "--init", "--recursive", "XenonRecomp-rs"]))?;
    }
    // The commit the index records: the one a checkout of this tree would fetch.
    let recorded = output(&mut git(&["rev-parse", ":XenonRecomp-rs"]))?;
    let recorded = recorded.trim();
    let head = output(&mut git(&["-C", "XenonRecomp-rs", "rev-parse", "HEAD"]))?;
    if head.trim() != recorded {
        let later = git(&["-C", "XenonRecomp-rs", "merge-base", "--is-ancestor", recorded, "HEAD"])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !later {
            return Err(format!(
                "XenonRecomp-rs/ is at {}, which does not contain {recorded}, the commit the tree records: \
                 git submodule update --recursive XenonRecomp-rs",
                head.trim()
            ));
        }
        println!("    XenonRecomp-rs/ is at {}, past the recorded {recorded}", head.trim());
    }
    Ok(())
}

fn xenon_build() -> Result {
    quiet(Command::new("cmake").args(["-B", XENON_BUILD, "-S", "XenonRecomp-rs", "-G", "Ninja"]).args([
        "-DCMAKE_BUILD_TYPE=Release",
        "-DCMAKE_C_COMPILER=clang-18",
        "-DCMAKE_CXX_COMPILER=clang++-18",
    ]))?;
    quiet(Command::new("cmake").args(["--build", XENON_BUILD, "-j", &jobs()]))
}

fn prepare(options: &Options, title: &Title) -> Result {
    println!("==> 1. XenonRecomp-rs");
    xenon_checkout()?;

    println!("==> 2. build XenonRecomp / XenonAnalyse");
    xenon_build()?;

    println!("==> 3. extract the executables from the ISO (XDVDFS)");
    let iso = Path::new(&options.iso);
    fs::create_dir_all(DISC).map_err(|e| format!("{DISC}: {e}"))?;
    if !Path::new(DISC).join("default.xex").is_file() || !Path::new(DISC).join("default_mp.xex").is_file() {
        if !iso.is_file() {
            return Err(format!(
                "no ISO at {}, and no {DISC}/default.xex + {DISC}/default_mp.xex: --iso <path>",
                options.iso
            ));
        }
        disc::extract_named(iso, Path::new(DISC), &["default.xex", "default_mp.xex"])?;
    }

    // Every fastfile on the disc, ~5.9 GB with the .pak archives beside them. A missing
    // one is not a missing level: the title asks for the zone, is told the file is not
    // there, reports a dirty disc and then falls over. Extracting the ones a particular
    // map needs means knowing them in advance, which nothing does -- the multiplayer
    // picks its own map. The Bink movies come too (~1.3 GB): the logos, and each
    // level's briefing, which plays while it loads. Files already there are left alone.
    fs::create_dir_all("mw2/game").map_err(|e| format!("mw2/game: {e}"))?;
    if iso.is_file() {
        disc::extract_game(iso, Path::new("mw2/game"))?;
    } else {
        println!("    no ISO: building without the game data");
    }
    println!("    {} files in mw2/game", count_files("mw2/game"));

    if !options.tu0 {
        println!("==> 3b. apply the title update");
        fs::create_dir_all(UPDATE).map_err(|e| format!("{UPDATE}: {e}"))?;
        let update = Path::new(UPDATE);
        if !update.join("default.xexp").is_file() || !update.join("default_mp.xexp").is_file() {
            let Some(tu) = options.tu.as_deref().map(Path::new).filter(|tu| tu.exists()) else {
                return Err(format!(
                    "no {UPDATE}/default.xexp: --tu <the update's package, or a folder with its files>"
                ));
            };
            if tu.is_dir() {
                for name in listing(tu)? {
                    if name.ends_with(".xexp") || name.ends_with(".ff") {
                        fs::copy(tu.join(&name), update.join(&name)).map_err(|e| format!("{name}: {e}"))?;
                    }
                }
            } else {
                disc::extract_package(tu, update)?;
            }
            // A folder without them, or another title's package, says so here rather
            // than at the patch.
            if !update.join("default.xexp").is_file() || !update.join("default_mp.xexp").is_file() {
                return Err(format!("{} gave no default.xexp and default_mp.xexp", tu.display()));
            }
        }
        for name in ["default", "default_mp"] {
            let patched = PathBuf::from(format!("mw2/{name}.xex"));
            if !patched.is_file() {
                disc::write_patched(
                    &Path::new(DISC).join(format!("{name}.xex")),
                    &update.join(format!("{name}.xexp")),
                    &patched,
                )?;
            }
        }
        // The update's fastfiles go with the disc's.
        for name in listing(update)? {
            let game = Path::new("mw2/game").join(&name);
            if name.ends_with(".ff") && !game.exists() {
                fs::copy(update.join(&name), &game).map_err(|e| format!("{name}: {e}"))?;
            }
        }
        println!("    {} files in mw2/game", count_files("mw2/game"));
    } else {
        // The disc's executables get a game folder without the update's fastfiles: the
        // disc's multiplayer would load a patch_mp.ff it found.
        let game = Path::new(DISC).join("game");
        fs::create_dir_all(&game).map_err(|e| format!("{}: {e}", game.display()))?;
        for name in listing(Path::new("mw2/game"))? {
            let link = game.join(&name);
            if Path::new("mw2/game").join(&name).exists() && !Path::new(UPDATE).join(&name).exists() && !link.exists() {
                std::os::unix::fs::symlink(Path::new("../../game").join(&name), &link)
                    .map_err(|e| format!("{}: {e}", link.display()))?;
            }
        }
        println!("    {} files in {DISC}/game", count_files(&format!("{DISC}/game")));
    }

    println!("==> 4. decrypt/decompress XEX -> flat PE memory image (for analysis)");
    // Both titles' images, whichever this build is, and the disc's too: the launcher, the
    // same for both, knows an install by the images of the version it installs and the
    // disc by its own (launcher/build.rs), which every region's disc shares. The release
    // workflow builds the launcher with the campaign, before the multiplayer is prepared.
    let folders = if title.folder == DISC { vec![DISC] } else { vec![title.folder, DISC] };
    for folder in folders {
        for (xex, pe) in [("default.xex", "default.pe"), ("default_mp.xex", "default_mp.pe")] {
            let pe = Path::new(folder).join(pe);
            if !pe.exists() {
                disc::write_image(&Path::new(folder).join(xex), &pe)?;
            }
        }
    }
    Ok(())
}

fn xenon_program(name: &str) -> PathBuf {
    root().join(XENON_BUILD).join(name).join(name)
}

// XenonRecomp runs in config/ with the paths it always had: they reach its output.
fn xenon_recomp(title: &Title, extra: &[String]) -> Command {
    let mut command = Command::new(xenon_program("XenonRecomp"));
    command.current_dir("config").args(extra).arg(&title.toml).arg("../XenonRecomp-rs/XenonUtils/ppc_context.h");
    command
}

fn last_line(text: &str) -> &str {
    text.lines().last().unwrap_or("")
}

// Cargo.lock lists a chunk crate per number up to the title's rust_chunk_count, the
// same in every title: a tree with another number (a XenonRecomp older than the
// option, or a tree recompiled before it) stops here rather than at cargo's lock check.
fn check_chunk_count(title: &Title, rust: &str) -> Result {
    let toml = format!("config/{}", title.toml);
    let text = fs::read_to_string(&toml).map_err(|e| format!("{toml}: {e}"))?;
    let wanted = text
        .lines()
        .find_map(|line| {
            line.trim().strip_prefix("rust_chunk_count")?.trim().strip_prefix('=')?.trim().parse::<usize>().ok()
        })
        .ok_or_else(|| format!("{toml} sets no rust_chunk_count"))?;
    let chunks = Path::new(rust).join("chunks");
    let found = fs::read_dir(&chunks).map_err(|e| format!("{}: {e}", chunks.display()))?.count();
    if found != wanted {
        return Err(format!(
            "{rust}/ has {found} chunk crates and {toml} asks for {wanted}: recompile it (without --keep-generated)"
        ));
    }
    Ok(())
}

fn recompile(options: &Options, title: &Title) -> Result {
    let rust = format!("{}_rs", title.ppc);
    if options.keep_generated && Path::new(&rust).join("Cargo.toml").exists() {
        println!("==> 5-6. keeping the recompiled tree in {rust}/");
    } else {
        println!("==> 5. detect jump tables");
        run(Command::new(xenon_program("XenonAnalyse"))
            .arg(format!("{}/{}", title.folder, title.xex))
            .arg(&title.tables))?;
        let tables = fs::read_to_string(&title.tables).map_err(|e| format!("{}: {e}", title.tables))?;
        println!("    tables: {}", tables.lines().filter(|line| line.starts_with("[[switch]]")).count());

        println!("==> 6. recompile PPC -> Rust");
        // Every function in Rust: one the backend would leave to C++ is an error.
        let extra = [
            "--backend=rust".into(),
            format!("--out=../{rust}"),
            "--rust-hooks=mw2_hooks.rs".into(),
            "--no-cpp-fallback".into(),
        ];
        let log = recompile_log(&mut xenon_recomp(title, &extra), &format!("{XENON_BUILD}/{rust}.log"))?;
        println!("{}", last_line(&log));
        let count = |text: &str| log.lines().filter(|line| line.contains(text)).count();
        println!("    unrecognized instructions: {}", count("Unrecognized instruction"));
        println!("    switch-case errors:        {}", count("trying to jump outside function"));
        for line in log.lines().filter(|line| line.starts_with("Registers stay in the context")) {
            println!("    {line}");
        }
    }
    println!("==> 7. kernel import stubs (into {rust}/)");
    generate::kernel_stubs(Path::new(&rust))?;
    check_chunk_count(title, &rust)?;
    let link = Path::new("generated");
    if fs::read_link(link).ok().as_deref() != Some(Path::new(&rust)) {
        if link.symlink_metadata().is_ok() {
            fs::remove_file(link).map_err(|e| format!("generated: {e}"))?;
        }
        std::os::unix::fs::symlink(&rust, link).map_err(|e| format!("generated: {e}"))?;
    }
    // Written only when it changes: build.rs takes a change for a new title.
    write_if_changed("generated.title", &format!("{} {}\n", title.name, title.version))?;
    Ok(())
}

fn build(options: &Options, title: &Title) -> Result {
    let windows = options.windows;
    let mut build = match title.name.as_str() {
        "mp" => "build-mp".to_string(),
        _ => "build".to_string(),
    };
    if options.release {
        build += "-release";
    }
    if windows {
        build = build.replacen("build", "build-win", 1);
    }
    if options.tu0 {
        build += "-tu0";
    }
    let build = options.build_dir.clone().unwrap_or(build);
    let build_path = root().join(&build);
    let exe = if windows { "mw2.exe" } else { "mw2" };
    let portable = if options.release { "ON" } else { "OFF" };
    let llvm_mingw = options.llvm_mingw.as_deref().unwrap_or_default();
    let toolchain: Vec<String> = if windows {
        vec![
            format!("-DCMAKE_TOOLCHAIN_FILE={}/cmake/mingw-w64.cmake", root().display()),
            format!("-DLLVM_MINGW={llvm_mingw}"),
        ]
    } else {
        vec!["-DCMAKE_C_COMPILER=clang-18".into(), "-DCMAKE_CXX_COMPILER=clang++-18".into()]
    };

    println!("==> 8. build the runtime + recompiled game");
    let configure = || {
        let mut command = Command::new("cmake");
        command.args(["-B", &build, "-G", "Ninja", "-DCMAKE_BUILD_TYPE=Release"]);
        command
            .arg(format!("-DMW2_VERSION={}", title.version))
            .arg(format!("-DMW2_PORTABLE={portable}"))
            .args(&options.cmake_extra)
            .args(&toolchain);
        // The game's ThinLTO cache is the launcher's too.
        if let Some(cache) = std::env::var_os("MW2_LTO_CACHE") {
            let mut define = std::ffi::OsString::from("-DMW2_LTO_CACHE=");
            define.push(cache);
            command.arg(define);
        }
        command
    };

    let features = if options.release { "portable" } else { "diagnostics" };
    // rustup's cargo, which takes the compiler rust-toolchain.toml pins: not the one
    // `cargo +<toolchain> xtask` would hand down.
    let cargo_command = || {
        let mut command = Command::new("cargo");
        command.env_remove("RUSTUP_TOOLCHAIN").env_remove("RUSTUP_TOOLCHAIN_SOURCE").env_remove("CARGO");
        command
    };
    let rustc_command = || {
        let mut command = Command::new("rustc");
        command.env_remove("RUSTUP_TOOLCHAIN").env_remove("RUSTUP_TOOLCHAIN_SOURCE");
        command
    };
    // A cargo build of this build's: `package` its package and binary, and their
    // features.
    let cargo_for = |package: &[&str]| -> Result<Command> {
        let mut cargo = cargo_command();
        cargo
            .args(["build", "--release", "--offline", "--locked"])
            .args(package)
            .env("CARGO_TARGET_DIR", build_path.join("rust"))
            .env("MW2_BUILD_DIR", &build_path)
            .env("MW2_RELEASE_TAG", &options.release_tag)
            // ash's build script would add the Vulkan SDK's folder to the link: the
            // game links the system's loader.
            .env_remove("VULKAN_SDK");
        // With rustup's rust-src component installed (an IDE adds it), rustc names the
        // standard library's own files by their local path, the user's home folder in
        // it, where it otherwise writes /rustc/<commit>: in every panic location of std
        // code built into the game. The same name either way, and no home folder in a
        // release.
        let rustc_info = output(rustc_command().arg("-vV"))?;
        let field = |name: &str| {
            rustc_info
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .map(str::trim)
                .ok_or(format!("rustc -vV: no {name}"))
        };
        let (commit, host) = (field("commit-hash:")?, field("host:")?);
        let sysroot = output(rustc_command().args(["--print", "sysroot"]))?;
        let remap = format!("--remap-path-prefix={}/lib/rustlib/src/rust=/rustc/{commit}", sysroot.trim());
        let triple = if windows { "x86_64-pc-windows-gnullvm" } else { host };
        // Cargo adds a --config array to the one .cargo/config.toml gives the target.
        cargo.arg("--config").arg(format!("target.{triple}.rustflags=[{remap:?}]"));
        if windows {
            cargo.args(["--target", "x86_64-pc-windows-gnullvm"]);
            cargo.env("LLVM_MINGW", llvm_mingw).env(
                "CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER",
                format!("{llvm_mingw}/bin/x86_64-w64-mingw32-clang"),
            );
        }
        Ok(cargo)
    };
    let rust_dir = if windows {
        build_path.join("rust/x86_64-pc-windows-gnullvm/release")
    } else {
        build_path.join("rust/release")
    };
    // An executable cargo made, under its name in the build folder.
    let place = |name: &str| -> Result {
        let built = build_path.join(name);
        if built.symlink_metadata().is_ok() {
            fs::remove_file(&built).map_err(|e| format!("{}: {e}", built.display()))?;
        }
        fs::hard_link(rust_dir.join(name), &built).map_err(|e| format!("{}: {e}", built.display()))
    };
    let mut cargo = cargo_for(&["--no-default-features", "--features", features])?;
    // The build itself is offline, and sdl3-sys and SDL's source come from the
    // registry, at the versions Cargo.lock holds: a build that would change it
    // stops.
    run(cargo_command().args(["fetch", "--locked"]))?;
    let out_dir = cargo_build(&mut cargo)?;
    // What build.rs left for this build, from the run cargo used: cargo keeps a
    // build-script run per feature set, and an earlier one is not run again, so files
    // it wrote into the folder itself could be another build's.
    for file in ["link.rsp", "build.txt"] {
        fs::copy(out_dir.join(file), build_path.join(file)).map_err(|e| format!("{file}: {e}"))?;
    }
    fs::write(build_path.join("out_dir.txt"), format!("{}\n", out_dir.display()))
        .map_err(|e| format!("out_dir.txt: {e}"))?;
    place(exe)?;
    check_hooks::check(&build_path)?;
    // The launcher and translate-shader are cargo's too; the rest is CMake's: what
    // --targets names besides them (everything when it names nothing), and on Linux the
    // zlib tools/inflate_diff.py holds the Rust's gunzip to.
    let every = options.targets.is_empty();
    let named = |name: &str| options.targets.iter().any(|target| target == name);
    let launcher = every || named("launcher");
    let translator = (every && !windows) || named("translate-shader");
    let mut rest: Vec<String> = if every { vec!["all".into()] } else { options.targets.clone() };
    rest.retain(|target| !matches!(target.as_str(), "mw2" | "launcher" | "translate-shader"));
    if !windows {
        rest.push("zlibstatic".into());
    }
    if !rest.is_empty() || launcher {
        quiet(&mut configure())?;
    }
    if !rest.is_empty() {
        run(Command::new("cmake").args(["--build", &build, "-j", &jobs(), "--target"]).args(&rest))?;
    }
    // The launcher: its build.rs builds launcher_core and its libraries in the folder just
    // configured, with the SDL sdl3-sys built, which only it knows.
    if launcher {
        let launcher_exe = if windows { "mw2-launcher.exe" } else { "mw2-launcher" };
        run(&mut cargo_for(&["-p", "mw2-launcher", "--no-default-features", "--features", features])?)?;
        place(launcher_exe)?;
    }
    // The translator on its own (shaders/translate_shader.rs).
    if translator {
        let translator_exe = if windows { "translate-shader.exe" } else { "translate-shader" };
        run(&mut cargo_for(&["-p", "mw2-shaders", "--bin", "translate-shader"])?)?;
        place(translator_exe)?;
    }
    let built = build_path.join(exe);
    let size = fs::metadata(&built).map_err(|e| format!("{}: {e}", built.display()))?.len();
    println!("{} ({size} bytes)", built.display());
    println!();
    println!("run it with:  ./{build}/{exe} {}/{} {}/game", title.folder, title.pe, title.folder);
    Ok(())
}

fn main() -> ExitCode {
    let options = match parse() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let title = Title::new(&options);
    let result = (|| {
        std::env::set_current_dir(root()).map_err(|e| format!("{}: {e}", root().display()))?;
        if options.windows && !options.llvm_mingw.as_deref().is_some_and(|dir| Path::new(dir).is_dir()) {
            return Err("--windows needs --llvm-mingw <llvm-mingw folder> (or LLVM_MINGW)".into());
        }
        // What the tools the build runs take the title from (tools/title.py).
        // SAFETY: nothing else runs yet.
        unsafe {
            std::env::set_var("MW2_TITLE", &title.name);
            std::env::set_var("MW2_VERSION", title.version);
        }
        prepare(&options, &title)?;
        if options.step == Step::Prepare {
            return Ok(());
        }
        recompile(&options, &title)?;
        if options.step == Step::Recompile {
            return Ok(());
        }
        build(&options, &title)
    })();
    std::io::stdout().flush().ok();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}
