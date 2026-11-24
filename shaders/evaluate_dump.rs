// evaluate.py's translator: the shader folder evaluate.py copies beside this file as
// `shader/` (shaders/src/shader, or the one --src names), compiled exactly as it stands,
// with nothing of the crate around it.
//
//     evaluate_dump <list> <folder>
//
// Every program of the list (a path a line, big-endian microcode, "pixel" in a pixel
// shader's file name) translated unscaled and scaled, its SPIR-V written to the folder as
// <line, from 0, in 6 digits>_<pixel|vertex>_<0|1>.spv. A program that does not translate
// writes nothing.
#![deny(unsafe_code)]

#[allow(dead_code)]
mod shader;

use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: evaluate_dump <list> <folder>");
        std::process::exit(2);
    }
    let list = std::fs::read_to_string(&args[1]).unwrap_or_else(|e| panic!("{}: {e}", args[1]));
    let mut written = 0;
    for (n, line) in list.lines().map(|line| line.trim_end_matches('\r')).filter(|line| !line.is_empty()).enumerate() {
        let bytes = std::fs::read(line).unwrap_or_else(|e| panic!("{line}: {e}"));
        let words: Vec<u32> = bytes.chunks_exact(4).map(|w| u32::from_be_bytes([w[0], w[1], w[2], w[3]])).collect();
        let name = Path::new(line).file_name().unwrap().to_string_lossy().into_owned();
        let pixel = name.contains("pixel");
        for scaled in [false, true] {
            let result = shader::translate(pixel, &words, scaled);
            if result.spirv.is_empty() {
                continue;
            }
            let kind = if pixel { "pixel" } else { "vertex" };
            let path = Path::new(&args[2]).join(format!("{n:06}_{kind}_{}.spv", u8::from(scaled)));
            let data: Vec<u8> = result.spirv.iter().flat_map(|word| word.to_le_bytes()).collect();
            std::fs::write(&path, data).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            written += 1;
        }
    }
    println!("dump: {written} modules");
}
