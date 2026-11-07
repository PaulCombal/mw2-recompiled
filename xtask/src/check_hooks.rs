// Every hook the build holds has to have won the link: a strong sub_X in an object
// replaces the guest code's weak one, and when it does not the game still links, and
// runs the original. Fails if a strong definition in the objects of <build>/link.rsp
// is not the one in <build>/mw2, or if the hooks do not add up to the number this
// title has: a hook ported to Rust moves from one count to the other, and one that
// went missing on the way changes the sum. (tools/check_hooks.py did this.)

use crate::Result;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::process::Command;

// Hooks per title, version and diagnostics. A build that has a new one says so here.
const EXPECTED: [(&str, &str, bool, usize); 6] = [
    ("sp", "tu6", true, 28),
    ("sp", "tu6", false, 24),
    ("mp", "tu6", true, 61),
    ("mp", "tu6", false, 54),
    ("sp", "tu0", true, 15),
    ("mp", "tu0", true, 17),
];

fn guest(name: &str) -> bool {
    name.len() == 12
        && name.starts_with("sub_")
        && name[4..].bytes().all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
}

fn tool(program: &str, args: &[&str]) -> Result<String> {
    crate::output(Command::new(program).args(args))
}

// Apple's nm (llvm's, in Xcode) lists a symbol as "<address> (<segment>,<section>)
// [weak ]external <name>", the name with the leading underscore Mach-O gives every C
// symbol, and says "weak external" of a weak definition where other formats have a
// letter: T is a strong one, W a weak one.
#[cfg(target_os = "macos")]
fn nm(paths: &[&str]) -> Result<Vec<(String, String, String)>> {
    let mut args = vec!["-m", "-U"];
    args.extend(paths);
    Ok(tool("nm", &args)?
        .lines()
        .filter_map(|line| {
            let name = line.split_whitespace().last()?.strip_prefix('_')?;
            let kind = if line.contains(" weak external ") { "W" } else { "T" };
            let address = line.split_whitespace().next()?;
            Some((name.to_string(), kind.to_string(), address.to_string()))
        })
        .collect())
}

#[cfg(not(target_os = "macos"))]
fn nm(paths: &[&str]) -> Result<Vec<(String, String, String)>> {
    let mut args = vec!["--defined-only"];
    args.extend(paths);
    Ok(tool("llvm-nm-18", &args)?
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let [.., kind, name] = parts[..] else { return None };
            Some((name.to_string(), kind.to_string(), parts[0].to_string()))
        })
        .collect())
}

// sub_X -> its kind (T for a strong definition), the last file's where several have one.
fn symbols(paths: &[String]) -> Result<HashMap<String, String>> {
    let mut found = HashMap::new();
    for chunk in paths.chunks(100) {
        let chunk: Vec<&str> = chunk.iter().map(String::as_str).collect();
        for (name, kind, _) in nm(&chunk)? {
            if guest(&name) {
                found.insert(name, kind);
            }
        }
    }
    Ok(found)
}

fn defined(path: &str) -> Result<Vec<(String, String)>> {
    let listing = tool("llvm-nm-18", &["--defined-only", path])?;
    Ok(listing
        .lines()
        .filter_map(|line| line.split_whitespace().last().map(|name| (name.to_string(), line.to_string())))
        .collect())
}

// What runs on a processor too old for the game may use nothing it lacks, in a crate
// compiled for one that has (src/runtime/cpu_level.rs).
fn check_processor(path: &str) -> Result<HashSet<String>> {
    // Only x86-64 has a check: on arm64 every processor does NEON.
    if cfg!(target_os = "macos") {
        return Ok(HashSet::new());
    }
    let names: HashSet<String> = defined(path)?
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| name == "mw2_check_processor" || name.ends_with("too_old"))
        .collect();
    for name in &names {
        let check = tool("llvm-objdump-18", &["-d", &format!("--disassemble-symbols={name}"), path])?;
        if !check.contains(name.as_str()) {
            return Err(format!("{name} did not disassemble, so nothing was checked"));
        }
        if ["xmm", "ymm", "popcnt", "pshufb"].iter().any(|instruction| check.contains(instruction)) {
            return Err(format!("{name} uses an instruction a processor it refuses may not have"));
        }
    }
    Ok(names)
}

// Windows runs a constructor from a table, and leaves one out without a word: the
// table is the only place that holds the check's full address.
fn check_windows(path: &str) -> Result {
    if !check_processor(path)?.contains("mw2_check_processor") {
        return Ok(());
    }
    let line =
        defined(path)?.into_iter().find(|(_, line)| line.ends_with(" mw2_check_processor")).map(|(_, line)| line);
    let address = line
        .and_then(|line| u64::from_str_radix(line.split_whitespace().next()?, 16).ok())
        .ok_or("mw2_check_processor has no address")?;
    let image = fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    if !image.windows(8).any(|bytes| bytes == address.to_le_bytes()) {
        return Err("mw2_check_processor is not in the table of constructors: it would never run".into());
    }
    Ok(())
}

pub fn check(build: &Path) -> Result {
    let exe = build.join("mw2.exe");
    if exe.exists() {
        check_windows(&exe.to_string_lossy())?;
        println!("    processor check: fine");
        return Ok(());
    }
    let read =
        |name: &str| fs::read_to_string(build.join(name)).map_err(|e| format!("{}/{name}: {e}", build.display()));
    let objects: Vec<String> =
        read("link.rsp")?.lines().map(str::trim).filter(|line| line.ends_with(".o")).map(String::from).collect();
    let hooks: HashSet<String> =
        symbols(&objects)?.into_iter().filter(|(_, kind)| kind == "T").map(|(name, _)| name).collect();
    let mw2 = build.join("mw2").to_string_lossy().into_owned();
    let all = symbols(std::slice::from_ref(&mw2))?;
    let strong: HashSet<&String> = all.iter().filter(|(_, kind)| *kind == "T").map(|(name, _)| name).collect();

    let mut lost: Vec<&String> = hooks.iter().filter(|hook| !strong.contains(hook)).collect();
    lost.sort();
    if !lost.is_empty() {
        let first: Vec<&str> = lost.iter().take(10).map(|name| name.as_str()).collect();
        return Err(format!("{} hooks lost at the link, the original runs instead: {}", lost.len(), first.join(" ")));
    }
    // What build.rs wrote down of this build: "<title> <version> diagnostics=<0|1>".
    let built = read("build.txt")?;
    let [title, version, diagnostics] = built.split_whitespace().collect::<Vec<_>>()[..] else {
        return Err(format!("{}/build.txt is not \"<title> <version> diagnostics=<0|1>\"", build.display()));
    };
    let diagnostics = diagnostics == "diagnostics=1";
    let expected = EXPECTED.iter().find(|entry| (entry.0, entry.1, entry.2) == (title, version, diagnostics));
    let python_bool = if diagnostics { "True" } else { "False" };
    match expected {
        None => println!(
            "    no hook count on record for {title} {version} diagnostics={python_bool}: {} here",
            strong.len()
        ),
        Some(&(.., expected)) if strong.len() != expected => {
            return Err(format!("{} hooks in the executable, and this build has {expected}", strong.len()));
        }
        Some(_) => {}
    }
    check_processor(&mw2)?;
    let from_rust = strong.iter().filter(|name| !hooks.contains(name.as_str())).count();
    println!(
        "    hooks: {} from the objects, {from_rust} from Rust, {} guest functions left as they are",
        hooks.len(),
        all.len() - strong.len()
    );
    Ok(())
}
