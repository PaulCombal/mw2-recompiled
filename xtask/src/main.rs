// The build, from the retail disc to the executables:
//
//   cargo xtask prepare   [options]   XenonRecomp built, the executables and the game
//                                     data taken from the disc, the title update
//                                     applied, the flat image written
//   cargo xtask recompile [options]   and the guest code recompiled to C++ and Rust
//   cargo xtask build     [options]   and the game, the launcher and the tools built
//
// Each step does only what is not done yet, but the recompilation, which runs every
// time unless --keep-generated. Run from the repository's root (cargo finds the alias
// in .cargo/config.toml there). docs/building.md has the options' whole story.

mod check_hooks;
mod generate;
mod stfs;
mod xdvdfs;
mod xexdump;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

type Result<T = ()> = std::result::Result<T, String>;

const USAGE: &str = "usage: cargo xtask <prepare|recompile|build> [options]

  --title sp|mp          sp (default): default.xex, the campaign and special ops;
                         mp: default_mp.xex, the multiplayer
  --tu0                  the disc's own executables instead of title update 6's
  --release              without diagnostics, and on Linux the C++ library linked in
  --windows              cross-compile for Windows with llvm-mingw
  --llvm-mingw <dir>     llvm-mingw's folder (or LLVM_MINGW in the environment)
  --cpp                  the C++ build of the game, with CMake, instead of cargo's
  --renderer rust|cpp    rust (default): the renderer's files Rust has taken over are
                         its; cpp: today's C++ renderer whole, in <build>-cppgpu
  --build-dir <dir>      build there instead of build, build-mp, build-release, ...
  --targets <names>      build only these (mw2, launcher, ...), space-separated
  --release-tag <tag>    the release the build is published as (v0.3.0)
  --keep-generated       keep the recompiled trees already there
  --iso <path>           the retail disc image
  --tu <path>            title update 6's package, or a folder with its files
  --cmake-extra <arg>    passed to CMake's configure step; may be repeated

The game's options in a cargo build -- MW2_LOGGING, MW2_LTO, MW2_LTO_CACHE, MW2_PGO,
MW2_PGO_TRAINING -- are taken from the environment.";

const ISO: &str = "/home/paul/Téléchargements/Call of Duty - Modern Warfare 2 (USA, Europe).iso";
// The disc's executables, which the update patches, and the update's files.
const DISC: &str = "mw2/tu0";
const UPDATE: &str = "mw2/update";
// Where XenonRecomp, XenonAnalyse and xexpatch are built.
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
    cpp: bool,
    renderer: Option<String>,
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
        cpp: false,
        renderer: None,
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
            "--cpp" => options.cpp = true,
            "--renderer" => options.renderer = Some(value()?),
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
        if inline.is_some()
            && matches!(name.as_str(), "--tu0" | "--release" | "--windows" | "--cpp" | "--keep-generated")
        {
            return Err(format!("{name} takes no value"));
        }
    }
    if options.title != "sp" && options.title != "mp" {
        return Err(format!("--title must be sp or mp, not '{}'", options.title));
    }
    match options.renderer.as_deref() {
        None | Some("rust" | "cpp") => {}
        Some(other) => return Err(format!("--renderer must be rust or cpp, not '{other}'")),
    }
    if options.cpp && options.renderer.is_some() {
        return Err("--renderer is for cargo's build: --cpp is the C++ renderer already".into());
    }
    // The game's options are build.rs's, from the environment: in CMake's
    // arguments they would reach only the launcher and the tools.
    const OPTIONS: [&str; 5] = ["LOGGING", "LTO", "LTO_CACHE", "PGO", "PGO_TRAINING"];
    if !options.cpp
        && options.cmake_extra.iter().any(|arg| {
            OPTIONS.iter().any(|option| {
                let name = format!("-DMW2_{option}");
                arg.strip_prefix(&name).is_some_and(|rest| rest.starts_with([':', '=']))
            })
        })
    {
        return Err("a cargo build takes MW2_LOGGING, MW2_LTO, MW2_LTO_CACHE, MW2_PGO and MW2_PGO_TRAINING \
                    from the environment, not --cmake-extra"
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

// XenonRecomp's output, which it is not up to the build to hold to UTF-8.
fn recompile_log(command: &mut Command) -> Result<String> {
    let output = command.stderr(Stdio::inherit()).output();
    let output = output.map_err(|e| format!("{}: {e}", command.get_program().to_string_lossy()))?;
    if !output.status.success() {
        return Err(format!("{} failed ({})", command.get_program().to_string_lossy(), output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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

fn modified(path: &str) -> Option<std::time::SystemTime> {
    fs::metadata(path).and_then(|metadata| metadata.modified()).ok()
}

// XenonRecomp's own patcher, from the command line: XenonAnalyse reads the patched
// executable before XenonRecomp would make it. Built again when it or its library is
// newer.
fn xexpatch() -> Result<String> {
    let program = format!("{XENON_BUILD}/xexpatch");
    let utils = format!("{XENON_BUILD}/XenonUtils/libXenonUtils.a");
    let disasm = format!("{XENON_BUILD}/thirdparty/disasm/libdisasm.a");
    let built = modified(&program);
    if [&utils, &disasm, &"tools/xexpatch.cpp".into()].iter().any(|input| modified(input) >= built) {
        run(Command::new("clang++-18")
            .args(["-std=c++20", "-O1", "-IXenonRecomp-rs/XenonUtils", "-IXenonRecomp-rs/thirdparty/simde"])
            .args(["tools/xexpatch.cpp", &utils, &disasm, "-o", &program]))?;
    }
    Ok(program)
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
        xdvdfs::extract_named(iso, Path::new(DISC), &["default.xex", "default_mp.xex"])?;
    }

    // Every fastfile on the disc, ~5.9 GB with the .pak archives beside them. A missing
    // one is not a missing level: the title asks for the zone, is told the file is not
    // there, reports a dirty disc and then falls over. Extracting the ones a particular
    // map needs means knowing them in advance, which nothing does -- the multiplayer
    // picks its own map. The Bink movies come too (~1.3 GB): the logos, and each
    // level's briefing, which plays while it loads. Files already there are left alone.
    fs::create_dir_all("mw2/game").map_err(|e| format!("mw2/game: {e}"))?;
    if iso.is_file() {
        xdvdfs::extract_game(iso, Path::new("mw2/game"))?;
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
                stfs::extract(tu, update)?;
            }
            // A folder without them, or another title's package, says so here rather
            // than at the patch.
            if !update.join("default.xexp").is_file() || !update.join("default_mp.xexp").is_file() {
                return Err(format!("{} gave no default.xexp and default_mp.xexp", tu.display()));
            }
        }
        let patch = xexpatch()?;
        for name in ["default", "default_mp"] {
            let patched = format!("mw2/{name}.xex");
            if !Path::new(&patched).is_file() {
                run(Command::new(&patch)
                    .arg(format!("{DISC}/{name}.xex"))
                    .arg(format!("{UPDATE}/{name}.xexp"))
                    .arg(&patched))?;
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
    let pe = Path::new(title.folder).join(title.pe);
    if !pe.exists() {
        xexdump::write(&Path::new(title.folder).join(title.xex), &pe)?;
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

// `to` made a copy of the flat folder `from`: a file whose bytes differ (or is missing)
// written, one `from` does not have removed, and the others left alone with their times.
// A link or anything but a folder at `to` is replaced by one. `last`, the file
// --keep-generated takes for a whole tree, goes first and comes back last, so that a copy
// cut short is never taken for one.
fn sync_tree(from: &Path, to: &Path, last: &str) -> Result {
    let error = |path: &Path, e: std::io::Error| format!("{}: {e}", path.display());
    if to.symlink_metadata().is_ok_and(|meta| !meta.is_dir()) {
        fs::remove_file(to).map_err(|e| error(to, e))?;
    }
    fs::create_dir_all(to).map_err(|e| error(to, e))?;
    let names = |folder: &Path| -> Result<Vec<std::ffi::OsString>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(folder).map_err(|e| error(folder, e))? {
            names.push(entry.map_err(|e| error(folder, e))?.file_name());
        }
        Ok(names)
    };
    let wanted = names(from)?;
    let stale: Vec<_> = names(to)?.into_iter().filter(|name| !wanted.contains(name)).collect();
    let mut changed = Vec::new();
    for name in wanted {
        let bytes = fs::read(from.join(&name)).map_err(|e| error(&from.join(&name), e))?;
        if fs::read(to.join(&name)).ok().as_deref() != Some(bytes.as_slice()) {
            changed.push((name, bytes));
        }
    }
    if stale.is_empty() && changed.is_empty() {
        return Ok(());
    }
    // The marker goes first and, written whether it changed or not, comes back last.
    let marker = to.join(last);
    if marker.symlink_metadata().is_ok() {
        if from.join(last).exists() && !changed.iter().any(|(name, _)| name == last) {
            changed.push((last.into(), fs::read(from.join(last)).map_err(|e| error(&from.join(last), e))?));
        }
        fs::remove_file(&marker).map_err(|e| error(&marker, e))?;
    }
    changed.sort_by_key(|(name, _)| name == last);
    for name in stale {
        let path = to.join(&name);
        if path.symlink_metadata().is_ok_and(|meta| meta.is_dir()) {
            fs::remove_dir_all(&path).map_err(|e| error(&path, e))?;
        } else {
            fs::remove_file(&path).map_err(|e| error(&path, e))?;
        }
    }
    for (name, bytes) in changed {
        let target = to.join(&name);
        fs::write(&target, &bytes).map_err(|e| error(&target, e))?;
    }
    Ok(())
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
    let ppc = &title.ppc;
    if options.keep_generated && Path::new(ppc).join("ppc_func_mapping.cpp").exists() {
        println!("==> 5-7. keeping the recompiled tree in {ppc}/");
    } else {
        println!("==> 5. detect jump tables");
        run(Command::new(xenon_program("XenonAnalyse"))
            .arg(format!("{}/{}", title.folder, title.xex))
            .arg(&title.tables))?;
        let tables = fs::read_to_string(&title.tables).map_err(|e| format!("{}: {e}", title.tables))?;
        println!("    tables: {}", tables.lines().filter(|line| line.starts_with("[[switch]]")).count());

        println!("==> 6. recompile PPC -> C++");
        // Into a folder of its own, then copied over the tree a file at a time and only
        // where it changed (sync_tree): the renderer's C++ includes the tree's headers,
        // and a header rewritten the same would recompile it.
        let staging = format!("{ppc}.new");
        if Path::new(&staging).symlink_metadata().is_ok() {
            fs::remove_dir_all(&staging).map_err(|e| format!("{staging}: {e}"))?;
        }
        fs::create_dir_all(&staging).map_err(|e| format!("{staging}: {e}"))?;
        let log = recompile_log(&mut xenon_recomp(title, &[format!("--out=../{staging}")]))?;
        let log_path = format!("{XENON_BUILD}/{ppc}.log");
        fs::write(&log_path, &log).map_err(|e| format!("{log_path}: {e}"))?;
        println!("{}", last_line(&log));
        let count = |text: &str| log.lines().filter(|line| line.contains(text)).count();
        println!("    unrecognized instructions: {}", count("Unrecognized instruction"));
        println!("    switch-case errors:        {}", count("trying to jump outside function"));
        for line in log.lines().filter(|line| line.starts_with("Registers stay in the context")) {
            println!("    {line}");
        }

        println!("==> 7. generate declarations + kernel import stubs (into {ppc}/)");
        generate::shared(Path::new(&staging))?;
        generate::kernel_stubs(Path::new(&staging))?;
        sync_tree(Path::new(&staging), Path::new(ppc), "ppc_func_mapping.cpp")?;
        fs::remove_dir_all(&staging).map_err(|e| format!("{staging}: {e}"))?;
    }

    if !options.cpp {
        let rust = format!("{ppc}_rs");
        if options.keep_generated && Path::new(&rust).join("Cargo.toml").exists() {
            println!("==> 7b. keeping the recompiled Rust in {rust}/");
        } else {
            println!("==> 7b. recompile PPC -> Rust");
            let extra = ["--backend=rust".into(), format!("--out=../{rust}"), "--rust-hooks=mw2_hooks.rs".into()];
            println!("{}", last_line(&recompile_log(&mut xenon_recomp(title, &extra))?));
        }
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
    }
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
    let cpp_renderer = options.renderer.as_deref() == Some("cpp");
    if cpp_renderer {
        build += "-cppgpu";
    }
    let build = options.build_dir.clone().unwrap_or(build);
    let build_path = root().join(&build);
    let exe = if windows { "mw2.exe" } else { "mw2" };
    let (diagnostics, portable) = if options.release { ("OFF", "ON") } else { ("ON", "OFF") };
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
    // A second build directory reuses the sources the first one fetched.
    let mut fetched = Vec::new();
    if build != "build" {
        // Only the SDL this tree asks for: the launcher's, which must be the version
        // sdl3-sys builds the game's from (build.rs checks).
        let cmake_lists = fs::read_to_string("CMakeLists.txt").map_err(|e| format!("CMakeLists.txt: {e}"))?;
        let tag = cmake_lists.lines().find_map(|line| line.split_once("GIT_TAG release-").map(|(_, tag)| tag.trim()));
        let fetched_sdl = fs::read_to_string("build/_deps/sdl3-src/CMakeLists.txt").unwrap_or_default();
        if tag.is_some_and(|tag| fetched_sdl.contains(&format!("VERSION \"{tag}\""))) {
            fetched.push(format!("-DFETCHCONTENT_SOURCE_DIR_SDL3={}/build/_deps/sdl3-src", root().display()));
        }
        for (name, folder) in [("FFMPEG_XMA", "ffmpeg_xma-src"), ("IMGUI", "imgui-src")] {
            if Path::new("build/_deps").join(folder).is_dir() {
                fetched.push(format!("-DFETCHCONTENT_SOURCE_DIR_{name}={}/build/_deps/{folder}", root().display()));
            }
        }
    }
    let configure = |rust: bool| {
        let mut command = Command::new("cmake");
        command.args(["-B", &build, "-G", "Ninja", "-DCMAKE_BUILD_TYPE=Release"]);
        if rust {
            command.arg("-DMW2_RUST=ON");
        }
        command
            .arg(format!("-DMW2_TITLE={}", title.name))
            .arg(format!("-DMW2_VERSION={}", title.version))
            .arg(format!("-DMW2_DIAGNOSTICS={diagnostics}"))
            .arg(format!("-DMW2_PORTABLE={portable}"))
            .arg(format!("-DMW2_RELEASE_TAG={}", options.release_tag))
            .args(&fetched)
            .args(&options.cmake_extra);
        // CMake keeps the choice in the build directory: a C++ build has to clear it.
        if !rust {
            command.arg("-DMW2_RUST=OFF");
        }
        command.args(&toolchain);
        command
    };

    if options.cpp {
        quiet(&mut configure(false))?;
        let mut command = Command::new("cmake");
        command.args(["--build", &build, "-j", &jobs()]);
        if !options.targets.is_empty() {
            command.arg("--target").args(&options.targets);
        }
        run(&mut command)?;
    } else {
        let features = match (options.release, cpp_renderer) {
            (false, false) => "diagnostics,rust-renderer",
            (true, false) => "portable,rust-renderer",
            (false, true) => "diagnostics",
            (true, true) => "portable",
        };
        // rustup's cargo, which takes the compiler rust-toolchain.toml pins: not the one
        // `cargo +<toolchain> xtask` would hand down.
        let cargo_command = || {
            let mut command = Command::new("cargo");
            command.env_remove("RUSTUP_TOOLCHAIN").env_remove("RUSTUP_TOOLCHAIN_SOURCE").env_remove("CARGO");
            command
        };
        let mut cargo = cargo_command();
        cargo
            .args(["build", "--release", "--offline", "--locked", "--no-default-features", "--features", features])
            .env("CARGO_TARGET_DIR", build_path.join("rust"))
            .env("MW2_BUILD_DIR", &build_path)
            .env("MW2_RELEASE_TAG", &options.release_tag)
            // ash's build script would add the Vulkan SDK's folder to the link: the
            // game links the system's loader, as the C++ does.
            .env_remove("VULKAN_SDK");
        let mut rust_dir = build_path.join("rust/release");
        if windows {
            cargo.args(["--target", "x86_64-pc-windows-gnullvm"]);
            rust_dir = build_path.join("rust/x86_64-pc-windows-gnullvm/release");
            cargo.env("LLVM_MINGW", llvm_mingw).env(
                "CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER",
                format!("{llvm_mingw}/bin/x86_64-w64-mingw32-clang"),
            );
        }
        // The build itself is offline, and sdl3-sys and SDL's source come from the
        // registry, at the versions Cargo.lock holds: a build that would change it
        // stops.
        run(cargo_command().args(["fetch", "--locked"]))?;
        let out_dir = cargo_build(&mut cargo)?;
        // What build.rs left for this build, from the run cargo used: with two
        // renderers in one folder, a fresh run of the other feature set is not run
        // again, so files it wrote into the folder itself would be the other's.
        for file in ["link.rsp", "build.txt", "renderer_cfgs.txt"] {
            fs::copy(out_dir.join(file), build_path.join(file)).map_err(|e| format!("{file}: {e}"))?;
        }
        fs::write(build_path.join("out_dir.txt"), format!("{}\n", out_dir.display()))
            .map_err(|e| format!("out_dir.txt: {e}"))?;
        let built = build_path.join(exe);
        if built.symlink_metadata().is_ok() {
            fs::remove_file(&built).map_err(|e| format!("{}: {e}", built.display()))?;
        }
        fs::hard_link(rust_dir.join(exe), &built).map_err(|e| format!("{}: {e}", built.display()))?;
        check_hooks::check(&build_path)?;
        // The rest is CMake's: what --targets names besides the game (everything when
        // it names nothing), and on Linux the C libraries the rigs hold the Rust to.
        let mut rest: Vec<String> =
            if options.targets.is_empty() { vec!["all".into()] } else { options.targets.clone() };
        rest.retain(|target| target != "mw2");
        if !windows {
            rest.extend(["ffmpeg_xma".into(), "zlibstatic".into()]);
        }
        if !rest.is_empty() {
            quiet(&mut configure(true))?;
            run(Command::new("cmake").args(["--build", &build, "-j", &jobs(), "--target"]).args(&rest))?;
        }
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
