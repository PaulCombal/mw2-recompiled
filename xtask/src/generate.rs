// What the build writes beside XenonRecomp's C++, as tools/genshared.py and
// tools/gen_kernel_stubs.py did: the declaration of every guest function and kernel
// import (ppc_recomp_shared.h), the list of imports (kernel_imports.txt), and a
// logging stub for each import the runtime's C++ does not define
// (stubs_generated.cpp).

use crate::Result;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

fn word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

// An identifier of C: a letter or _, then letters, digits and _, as long as it goes.
fn identifier(text: &[u8], at: usize) -> Option<&[u8]> {
    if !text.get(at).is_some_and(|&byte| byte.is_ascii_alphabetic() || byte == b'_') {
        return None;
    }
    let length = text[at..].iter().take_while(|&&byte| word(byte)).count();
    Some(&text[at..at + length])
}

fn find_all<'a>(text: &'a [u8], needle: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
    (0..text.len().saturating_sub(needle.len() - 1)).filter(move |&at| text[at..].starts_with(needle))
}

fn name(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

// PPC_WEAK_FUNC(<identifier>), and PPC_FUNC_IMPL(<identifier>): the body each weak name
// calls, which is no kernel import. A linker that takes a weak stub in an object file
// for the definition (Mach-O's) never reads the real one from the archive.
fn definitions(text: &[u8], out: &mut BTreeSet<String>) {
    for marker in [&b"PPC_WEAK_FUNC("[..], &b"PPC_FUNC_IMPL("[..]] {
        for at in find_all(text, marker) {
            let start = at + marker.len();
            if let Some(found) = identifier(text, start)
                && text.get(start + found.len()) == Some(&b')')
            {
                out.insert(name(found));
            }
        }
    }
}

// <identifier>(ctx, base), the identifier whole, and { 0x<HEX>, <identifier> }.
fn references(text: &[u8], out: &mut BTreeSet<String>) {
    for at in find_all(text, b"(ctx, base)") {
        let start = at - text[..at].iter().rev().take_while(|&&byte| word(byte)).count();
        if let Some(found) = identifier(text, start).filter(|found| found.len() == at - start) {
            out.insert(name(found));
        }
    }
    for at in find_all(text, b"{ 0x") {
        let digits =
            text[at + 4..].iter().take_while(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(byte)).count();
        let start = at + 4 + digits;
        if digits == 0 || !text[start..].starts_with(b", ") {
            continue;
        }
        if let Some(found) = identifier(text, start + 2)
            && text[start + 2 + found.len()..].starts_with(b" }")
        {
            out.insert(name(found));
        }
    }
}

fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn write(path: &Path, text: &str) -> Result {
    fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

// A list of names as Python prints one.
fn python_list(names: &[&String]) -> String {
    let quoted: Vec<String> = names.iter().map(|name| format!("'{name}'")).collect();
    format!("[{}]", quoted.join(", "))
}

pub fn shared(ppc: &Path) -> Result {
    let mut files: Vec<_> = crate::listing(ppc)?
        .into_iter()
        .filter(|name| {
            name.starts_with("ppc_recomp.") && name.ends_with(".cpp") && name.len() >= "ppc_recomp..cpp".len()
        })
        .map(|name| ppc.join(name))
        .collect();
    files.push(ppc.join("ppc_func_mapping.cpp"));
    let (mut defined, mut referenced) = (BTreeSet::new(), BTreeSet::new());
    for file in &files {
        let text = read(file)?;
        definitions(&text, &mut defined);
        references(&text, &mut referenced);
    }
    referenced.remove("PPC_CALL_INDIRECT_FUNC");
    let all: BTreeSet<&String> = defined.union(&referenced).collect();

    let mut header = include_str!("ppc_recomp_shared.h.in").to_string();
    for name in &all {
        header += &format!("PPC_FUNC({name});\n");
    }
    header += "}\n";
    write(&ppc.join("ppc_recomp_shared.h"), &header)?;

    let outside: Vec<&String> = referenced.difference(&defined).collect();
    let imports: Vec<&str> = outside.iter().filter_map(|name| name.strip_prefix("__imp__")).collect();
    // The runtime supplies these; the list is kernel_stubs's, below.
    write(&ppc.join("kernel_imports.txt"), &(imports.join("\n") + "\n"))?;
    let missing: Vec<&String> = outside.iter().copied().filter(|name| !name.starts_with("__imp__")).collect();
    println!(
        "defined={} referenced={} declared={} kernel_imports={}",
        defined.len(),
        referenced.len(),
        all.len(),
        imports.len()
    );
    println!(
        "referenced-but-undefined non-import symbols: {} {}",
        missing.len(),
        python_list(&missing[..missing.len().min(10)])
    );
    Ok(())
}

// Every import the runtime does not implement logs once and returns 0, so a run
// reports exactly which ones the game reaches. An import counts as implemented once
// some runtime/kernel/*.cpp defines PPC_FUNC(__imp__<name>).
pub fn kernel_stubs(ppc: &Path) -> Result {
    let list = String::from_utf8_lossy(&read(&ppc.join("kernel_imports.txt"))?).into_owned();
    let listed: Vec<String> = list.lines().map(str::trim).filter(|line| !line.is_empty()).map(String::from).collect();
    let need: BTreeSet<String> = listed.iter().cloned().collect();
    let mut implemented = BTreeSet::new();
    let kernel = Path::new("runtime/kernel");
    for file in crate::listing(kernel)? {
        if !file.ends_with(".cpp") || file.ends_with("stubs_generated.cpp") {
            continue;
        }
        let text = read(&kernel.join(file))?;
        for at in find_all(&text, b"PPC_FUNC(__imp__") {
            let start = at + b"PPC_FUNC(__imp__".len();
            let length = text[start..].iter().take_while(|&&byte| word(byte)).count();
            if length > 0 && text.get(start + length) == Some(&b')') {
                implemented.insert(name(&text[start..start + length]));
            }
        }
    }
    let missing: Vec<&String> = need.difference(&implemented).collect();
    let mut out = String::from(
        "// Generated by cargo xtask (xtask/src/generate.rs) -- do not edit.\n\
         // Every import here logs once and returns 0, so a run reports exactly\n\
         // which ones the game actually reaches.\n\
         #include <ppc_recomp_shared.h>\n#include \"kernel/kernel.h\"\n\n",
    );
    for name in &missing {
        out += &format!("PPC_FUNC(__imp__{name}) {{ kernel::Unimplemented(\"{name}\"); ctx.r3.u64 = 0; }}\n");
    }
    write(&ppc.join("stubs_generated.cpp"), &out)?;
    println!(
        "kernel imports: {} needed, {} implemented, {} stubbed",
        listed.len(),
        need.intersection(&implemented).count(),
        missing.len()
    );
    let unknown: Vec<&String> = implemented.difference(&need).collect();
    if !unknown.is_empty() {
        println!("  note: implemented but not imported by the game: {}", python_list(&unknown));
    }
    Ok(())
}
