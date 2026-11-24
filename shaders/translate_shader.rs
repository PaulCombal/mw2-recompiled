// The shader translator on its own.
//
// The translator runs in the game, because the title hands the GPU microcode while it
// runs. But iterating on it by launching the game is slow, and the output wants checking
// with spirv-val, so the same code is also built as a small tool:
//
//     translate-shader [--compile] <file.ucode> [out.spv]
//
// The shader type comes from the file name, which is how MW2_DUMP_SHADERS writes them
// (vertex_*.ucode / pixel_*.ucode).
//
// --compile also hands the module to a real Vulkan driver. spirv-val checks the binary
// against the specification; the driver checks it against what it will actually
// compile, which is a stronger and less forgiving statement.
//
// The C runtime's main, files and console, as the tool was written in C++: its arguments
// as they are, perror's texts, printf's padding, stdout buffered as C's is, and nothing of
// Rust's start-up (SIGPIPE stays as it was).
#![no_main]

use mw2_shaders::{pipeline, shader};
use std::ffi::{CStr, CString, c_char, c_int, c_long, c_void};

unsafe extern "C" {
    fn fopen(path: *const c_char, mode: *const c_char) -> *mut c_void;
    fn fclose(file: *mut c_void) -> c_int;
    fn fseek(file: *mut c_void, offset: c_long, whence: c_int) -> c_int;
    fn ftell(file: *mut c_void) -> c_long;
    fn fread(buffer: *mut c_void, size: usize, count: usize, file: *mut c_void) -> usize;
    fn fwrite(buffer: *const c_void, size: usize, count: usize, file: *mut c_void) -> usize;
    fn perror(text: *const c_char);
    fn printf(format: *const c_char, ...) -> c_int;
    fn fputs(text: *const c_char, stream: *mut c_void) -> c_int;
    #[cfg(not(windows))]
    #[link_name = "stderr"]
    static STDERR: *mut c_void;
    #[cfg(windows)]
    fn __acrt_iob_func(index: u32) -> *mut c_void;
}

const SEEK_SET: c_int = 0;
const SEEK_END: c_int = 2;

// fprintf(stderr, ...) of a whole text.
fn error(text: &[u8]) {
    let text = CString::new(text).unwrap();
    // SAFETY: a C string, and the C library's stderr.
    unsafe {
        #[cfg(not(windows))]
        let stream = STDERR;
        #[cfg(windows)]
        let stream = __acrt_iob_func(2);
        fputs(text.as_ptr(), stream);
    }
}

fn run(args: &[&CStr]) -> c_int {
    let mut compile = false;
    let mut arg = 1;
    if args.len() > 1 && args[1].to_bytes() == b"--compile" {
        compile = true;
        arg = 2;
    }
    if args.len() <= arg {
        error(b"usage: translate-shader [--compile] <file.ucode> [out.spv]\n");
        return 2;
    }

    let path = args[arg];
    // SAFETY: C strings, and a FILE owned until fclose.
    let mut words = unsafe {
        let file = fopen(path.as_ptr(), c"rb".as_ptr());
        if file.is_null() {
            perror(path.as_ptr());
            return 2;
        }
        fseek(file, 0, SEEK_END);
        let size = ftell(file);
        fseek(file, 0, SEEK_SET);
        let mut words = vec![0u32; size as usize / 4];
        if fread(words.as_mut_ptr().cast(), 4, words.len(), file) != words.len() {
            fclose(file);
            error(&[path.to_bytes(), b": short read\n"].concat());
            return 2;
        }
        fclose(file);
        words
    };

    // The capture is written big-endian, the way the guest holds it.
    for word in &mut words {
        *word = word.swap_bytes();
    }

    // MW2_DUMP_SHADERS writes pixel_*.ucode; the fastfile extractor writes ps_*.xsh.
    // Both are pixel shaders, and translating one as a vertex shader fails in ways that
    // look like translator bugs.
    let name = path.to_bytes();
    let contains = |part: &[u8]| name.windows(part.len()).any(|window| window == part);
    let pixel = contains(b"pixel") || contains(b"ps_");
    let result = shader::translate(pixel, &words, false);

    let described = shader::describe_parts(result.ok, result.error.as_bytes(), result.spirv.len(), &result.meta);
    let described = CString::new(described).unwrap();
    // SAFETY: C strings for the format's two %s.
    unsafe { printf(c"%-40s %s\n".as_ptr(), path.as_ptr(), described.as_ptr()) };
    if !result.ok {
        return 1;
    }

    if compile {
        // SAFETY: a headless device of its own, shut down below.
        if !unsafe { pipeline::initialise(None) } {
            error(b"no Vulkan device to compile against\n");
            return 3;
        }
        let accepted = pipeline::create_module(&result.spirv);
        let verdict = CString::new(match accepted {
            Ok(()) => "accepted",
            Err(why) => why,
        })
        .unwrap();
        // SAFETY: C strings for the format's two %s.
        unsafe { printf(c"%-40s driver: %s\n".as_ptr(), c"".as_ptr(), verdict.as_ptr()) };
        // SAFETY: nothing made with the device is alive.
        unsafe { pipeline::shutdown() };
        if accepted.is_err() {
            return 1;
        }
    }

    if args.len() >= arg + 2 {
        // A glob in the wrong place turns argv[2] into another input file, and writing
        // over it destroys a capture that cannot be reproduced without another run of
        // the game.
        let out = args[arg + 1];
        if out.to_bytes().ends_with(b".ucode") {
            error(&[b"refusing to write over ", out.to_bytes(), b": that is an input\n"].concat());
            return 2;
        }
        // SAFETY: C strings, the words, and a FILE owned until fclose.
        unsafe {
            let file = fopen(out.as_ptr(), c"wb".as_ptr());
            if file.is_null() {
                perror(out.as_ptr());
                return 2;
            }
            fwrite(result.spirv.as_ptr().cast(), 4, result.spirv.len(), file);
            fclose(file);
        }
    }
    0
}

#[unsafe(no_mangle)]
extern "C" fn main(argc: c_int, argv: *const *const c_char) -> c_int {
    // SAFETY: the C runtime's `argc` C strings.
    let args: Vec<&CStr> = (0..argc as usize).map(|i| unsafe { CStr::from_ptr(*argv.add(i)) }).collect();
    run(&args)
}
