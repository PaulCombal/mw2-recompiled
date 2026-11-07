// The few things the runtime asks of the host system that Linux and Windows spell
// differently, for what Rust's own library has no word for.

use std::ffi::{CStr, c_char, c_int, c_void};

#[repr(C)]
#[derive(Default)]
pub struct Load {
    pub process: u64,
    pub busy: u64,
    pub total: u64,
}

#[repr(C)]
#[derive(Default)]
pub struct MemoryState {
    pub installed: u64,
    pub usable: u64,
    pub free: u64,
    pub commit_limit: u64,
    pub committed: u64,
    pub process_peak: u64,
    pub process_committed: u64,
}

pub use system::*;

/// A path as a C string has it: UTF-8 on Windows, the system's bytes elsewhere.
pub fn path(text: &CStr) -> std::path::PathBuf {
    #[cfg(not(windows))]
    {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::OsStr::from_bytes(text.to_bytes()).into()
    }
    #[cfg(windows)]
    {
        text.to_string_lossy().as_ref().into()
    }
}

/// A path the C library handed over from the system: the command line or the
/// environment. The system's bytes elsewhere; on Windows text in its ANSI code page,
/// which is how a C++ path made of the same text read it.
pub fn system_path(text: &CStr) -> std::path::PathBuf {
    #[cfg(not(windows))]
    {
        path(text)
    }
    #[cfg(windows)]
    {
        use crate::runtime::windows::{CP_ACP, wide_in};
        use std::os::windows::ffi::OsStringExt;
        let mut wide = wide_in(CP_ACP, text);
        wide.pop(); // its zero
        std::ffi::OsString::from_wide(&wide).into()
    }
}

/// A line to stderr as it stands, with no stamp or level.
pub fn print_line(text: &CStr) {
    let stream = standard_error();
    unsafe {
        fprintf(stream, c"%s\n".as_ptr(), text.as_ptr());
        fflush(stream);
    }
}

// The C library's stderr, which MW2_LOG_FILE moved, and printf.
unsafe extern "C" {
    fn fprintf(stream: *mut c_void, format: *const c_char, ...) -> c_int;
    fn fflush(stream: *mut c_void) -> c_int;
    #[link_name = "freopen"]
    fn reopen(path: *const c_char, mode: *const c_char, stream: *mut c_void) -> *mut c_void;
    fn setvbuf(stream: *mut c_void, buffer: *mut c_char, mode: c_int, size: usize) -> c_int;
    fn _exit(code: c_int) -> !;
}

/// MW2_LOG_FILE: the C library's stderr, which every line of the log goes to, into
/// `path` (anew, or after what it holds), and a line at a time, as stderr is to a
/// terminal: the file is read while the run goes on. Windows has no line buffering
/// (and takes the request for an invalid one), so there every write goes straight
/// through. False when the file could not be opened.
pub fn send_log_to(path: &CStr, append: bool) -> bool {
    let mode = if append { c"a" } else { c"w" };
    let stream = standard_error();
    if unsafe { reopen(path.as_ptr(), mode.as_ptr(), stream) }.is_null() {
        return false;
    }
    #[cfg(not(windows))]
    const MODE: c_int = 1; // _IOLBF
    #[cfg(windows)]
    const MODE: c_int = 4; // _IONBF
    unsafe { setvbuf(stream, std::ptr::null_mut(), MODE, 0) };
    true
}

/// Ends the process here, with what the C library holds written out and nothing else
/// run: no handler at exit and no static's destructor, as the guest's threads are
/// still running over them.
pub fn exit_now(code: i32) -> ! {
    unsafe {
        fflush(std::ptr::null_mut());
        _exit(code)
    }
}

#[cfg(not(windows))]
mod system {
    use super::*;
    use std::fs;
    use std::mem::MaybeUninit;
    use std::ptr;

    const PRIO_PROCESS: c_int = 0;
    const SCHED_IDLE: c_int = 5;
    const CLOCK_PROCESS_CPUTIME_ID: c_int = 2;
    const SC_CLK_TCK: c_int = 2;
    const SC_NPROCESSORS_ONLN: c_int = 84;
    const RTLD_NOW: c_int = 2;
    const RTLD_NOLOAD: c_int = 4;

    #[repr(C)]
    #[derive(Default)]
    struct Timespec {
        seconds: i64,
        nanoseconds: i64,
    }

    unsafe extern "C" {
        #[link_name = "stderr"]
        static STDERR: *mut c_void;
        fn gettid() -> c_int;
        fn getpid() -> c_int;
        fn pthread_self() -> usize;
        fn pthread_setname_np(thread: usize, name: *const c_char) -> c_int;
        fn pthread_setschedparam(thread: usize, policy: c_int, param: *const c_int) -> c_int;
        fn pthread_getcpuclockid(thread: usize, clock: *mut c_int) -> c_int;
        fn setpriority(which: c_int, who: u32, priority: c_int) -> c_int;
        fn clock_gettime(clock: c_int, time: *mut Timespec) -> c_int;
        fn sysconf(name: c_int) -> i64;
        fn backtrace(frames: *mut *mut c_void, size: c_int) -> c_int;
        fn backtrace_symbols_fd(frames: *const *mut c_void, size: c_int, fd: c_int);
        fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(library: *mut c_void, name: *const c_char) -> *mut c_void;
        fn pthread_attr_init(attributes: *mut PthreadAttributes) -> c_int;
        fn pthread_attr_getstacksize(attributes: *const PthreadAttributes, size: *mut usize) -> c_int;
        fn pthread_attr_destroy(attributes: *mut PthreadAttributes) -> c_int;
    }

    // pthread_attr_t, which glibc makes 56 bytes on x86-64.
    #[repr(C, align(8))]
    struct PthreadAttributes([u8; 64]);

    /// The host stack a thread gets when its creator names no size, as a C++
    /// std::thread's does: glibc's default, which RLIMIT_STACK sets.
    pub fn default_thread_stack() -> usize {
        let mut attributes = PthreadAttributes([0; 64]);
        let mut size = 0;
        unsafe {
            if pthread_attr_init(&mut attributes) != 0 {
                return 8 << 20;
            }
            // An attribute with no size of its own gives the default.
            if pthread_attr_getstacksize(&attributes, &mut size) != 0 {
                size = 8 << 20;
            }
            pthread_attr_destroy(&mut attributes);
        }
        size
    }

    fn nanoseconds(time: &Timespec) -> u64 {
        (time.seconds as u64).wrapping_mul(1_000_000_000).wrapping_add(time.nanoseconds as u64)
    }

    pub fn standard_error() -> *mut c_void {
        unsafe { STDERR }
    }

    pub fn initialise() {}

    pub fn thread_id() -> u32 {
        (unsafe { gettid() }) as u32
    }

    pub fn is_main_thread() -> bool {
        unsafe { gettid() == getpid() }
    }

    /// Up to fifteen characters are kept.
    pub fn set_thread_name(name: &CStr) {
        let mut short = [0u8; 16];
        let bytes = name.to_bytes();
        let kept = bytes.len().min(15);
        short[..kept].copy_from_slice(&bytes[..kept]);
        unsafe { pthread_setname_np(pthread_self(), short.as_ptr().cast()) };
    }

    pub fn lower_thread_priority() {
        unsafe { setpriority(PRIO_PROCESS, gettid() as u32, 10) };
    }

    pub fn idle_thread_priority() {
        let none: c_int = 0;
        if unsafe { pthread_setschedparam(pthread_self(), SCHED_IDLE, &none) } != 0 {
            lower_thread_priority();
        }
    }

    /// The calling thread's host stack to stderr. The recompiled functions are named
    /// sub_XXXXXXXX, so where symbols resolve this is the guest's stack. Called from the
    /// crash handler: nothing here allocates.
    pub fn print_backtrace(why: &CStr) {
        let mut frames = [std::ptr::null_mut(); 64];
        let n = unsafe { backtrace(frames.as_mut_ptr(), 64) };
        unsafe {
            fprintf(STDERR, c"\n[E] ---- %s (%d frames) ----\n".as_ptr(), why.as_ptr(), n);
            fflush(STDERR);
            backtrace_symbols_fd(frames.as_ptr(), n, 2);
            fprintf(STDERR, c"[E] --------------------------------\n".as_ptr());
            fflush(STDERR);
        }
    }

    /// As std::thread::hardware_concurrency says, whatever this process may use.
    pub fn processor_threads() -> u32 {
        (unsafe { sysconf(SC_NPROCESSORS_ONLN) }).max(0) as u32
    }

    pub fn thread_clock() -> usize {
        let mut clock = MaybeUninit::<c_int>::uninit();
        if unsafe { pthread_getcpuclockid(pthread_self(), clock.as_mut_ptr()) } != 0 {
            return 0;
        }
        // Shifted past 0, which says there is none.
        (unsafe { clock.assume_init() } as u32 as usize) + 1
    }

    /// The calling thread's processor time so far, in nanoseconds.
    #[cfg(feature = "diagnostics")]
    pub fn thread_cpu_time() -> u64 {
        const CLOCK_THREAD_CPUTIME_ID: c_int = 3;
        let mut time = Timespec::default();
        unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut time) };
        nanoseconds(&time)
    }

    pub fn thread_time(clock: usize) -> Option<u64> {
        let mut time = Timespec::default();
        (clock != 0 && unsafe { clock_gettime((clock - 1) as u32 as c_int, &mut time) } == 0)
            .then(|| nanoseconds(&time))
    }

    pub fn processor_load(load: &mut Load) -> bool {
        let mut time = Timespec::default();
        if unsafe { clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &mut time) } != 0 {
            return false;
        }
        load.process = nanoseconds(&time);
        // The first line sums every processor thread, in ticks of USER_HZ.
        let Ok(stat) = fs::read_to_string("/proc/stat") else { return false };
        let Some(first) = stat.strip_prefix("cpu") else { return false };
        let ticks: Vec<u64> = first.split_ascii_whitespace().take(8).map_while(|word| word.parse().ok()).collect();
        if ticks.len() < 4 {
            return false;
        }
        let tick = 1_000_000_000 / unsafe { sysconf(SC_CLK_TCK) } as u64;
        let of = |index: usize| ticks.get(index).copied().unwrap_or(0);
        load.total = ticks.iter().sum::<u64>() * tick;
        load.busy = load.total - (of(3) + of(4)) * tick;
        true
    }

    // A "Name: N kB" line of a /proc file, in bytes.
    fn kilobytes(path: &str, name: &str) -> u64 {
        let Ok(text) = fs::read_to_string(path) else { return 0 };
        let line = text.lines().find_map(|line| line.strip_prefix(name)?.strip_prefix(':'));
        line.and_then(|line| line.split_ascii_whitespace().next()?.parse::<u64>().ok()).unwrap_or(0) << 10
    }

    pub fn memory_state(memory: &mut MemoryState) {
        memory.usable = kilobytes("/proc/meminfo", "MemTotal");
        memory.free = kilobytes("/proc/meminfo", "MemAvailable");
        memory.process_peak = kilobytes("/proc/self/status", "VmHWM");
    }

    /// RTLD_LOCAL is 0.
    pub fn open_library(path: &CStr) -> *mut c_void {
        unsafe { dlopen(path.as_ptr(), RTLD_NOW) }
    }

    pub fn loaded_library(name: &CStr) -> *mut c_void {
        unsafe { dlopen(name.as_ptr(), RTLD_NOW | RTLD_NOLOAD) }
    }

    pub unsafe fn symbol(library: *mut c_void, name: &CStr) -> *mut c_void {
        unsafe { dlsym(library, name.as_ptr()) }
    }

    /// What tells this machine from others, as its system names it; empty when it
    /// does not.
    pub fn machine_id() -> Vec<u8> {
        let text = std::fs::read("/etc/machine-id").unwrap_or_default();
        text.split(|byte| byte.is_ascii_whitespace()).find(|word| !word.is_empty()).unwrap_or_default().to_vec()
    }

    // struct tm, glibc's.
    #[repr(C)]
    struct Tm {
        numbers: [c_int; 9],
        offset: i64,
        zone: *const c_char,
    }

    unsafe extern "C" {
        fn time(out: *mut i64) -> i64;
        fn localtime_r(time: *const i64, tm: *mut Tm) -> *mut Tm;
        fn getrandom(buffer: *mut c_void, size: usize, flags: u32) -> isize;
    }

    /// The hour and the minute on this machine's clock.
    pub fn local_hour_minute() -> (u32, u32) {
        let now = unsafe { time(ptr::null_mut()) };
        let mut tm = Tm { numbers: [0; 9], offset: 0, zone: ptr::null() };
        unsafe { localtime_r(&now, &mut tm) };
        (tm.numbers[2] as u32, tm.numbers[1] as u32)
    }

    /// A number from the system's random source.
    pub fn random() -> u64 {
        let mut value = 0u64;
        while unsafe { getrandom((&raw mut value).cast(), 8, 0) } != 8 {}
        value
    }

    /// Makes [address, address + len) read-only, or readable and writable again: what
    /// the GPU's memory watch asks (gpu/memory_watch.rs). Safe in a signal handler:
    /// mprotect is one system call, and this allocates nothing, logs nothing and takes
    /// no lock. (At the module's end, so that no line above it moves.)
    ///
    /// # Safety
    /// The range is page-aligned memory this process mapped, which nothing that
    /// cannot take the fault relies on being writable.
    #[cfg(feature = "rust-renderer")]
    #[inline]
    pub unsafe fn protect(address: *mut u8, len: usize, writable: bool) -> bool {
        unsafe extern "C" {
            fn mprotect(address: *mut c_void, len: usize, protection: c_int) -> c_int;
        }
        const PROT_READ: c_int = 1;
        const PROT_WRITE: c_int = 2;
        let protection = if writable { PROT_READ | PROT_WRITE } else { PROT_READ };
        // SAFETY: the caller's contract.
        unsafe { mprotect(address.cast(), len, protection) == 0 }
    }
}

#[cfg(windows)]
mod system {
    use super::*;
    use crate::runtime::windows::*;
    use std::mem::{size_of, transmute};
    use std::ptr;
    use std::sync::atomic::{AtomicU32, Ordering};

    static MAIN_THREAD: AtomicU32 = AtomicU32::new(0);

    unsafe extern "C" {
        fn __acrt_iob_func(index: u32) -> *mut c_void;
        fn freopen(path: *const c_char, mode: *const c_char, stream: *mut c_void) -> *mut c_void;
    }

    fn stream(index: u32) -> *mut c_void {
        unsafe { __acrt_iob_func(index) }
    }

    pub fn standard_error() -> *mut c_void {
        stream(2)
    }

    fn nanoseconds(time: FileTime) -> u64 {
        ((u64::from(time.high) << 32) | u64::from(time.low)).wrapping_mul(100)
    }

    pub fn initialise() {
        MAIN_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
        unsafe { timeBeginPeriod(1) };
        // A player's build has no console window of its own. Started from a terminal, it
        // writes there, as a console program would.
        if unsafe { GetConsoleWindow().is_null() && AttachConsole(ATTACH_PARENT_PROCESS) != 0 } {
            unsafe {
                freopen(c"CONOUT$".as_ptr(), c"w".as_ptr(), stream(1));
                freopen(c"CONOUT$".as_ptr(), c"w".as_ptr(), stream(2));
            }
        }
    }

    pub fn thread_id() -> u32 {
        unsafe { GetCurrentThreadId() }
    }

    /// The host stack a thread gets when its creator names no size, as a C++
    /// std::thread's does: what the executable asks for (16 MB, CMakeLists.txt).
    pub fn default_thread_stack() -> usize {
        // SizeOfStackReserve, in the PE32+ optional header that follows the 24 bytes
        // of the signature and the file header.
        unsafe {
            let image = GetModuleHandleW(ptr::null()).cast::<u8>().cast_const();
            let header = image.add(ptr::read_unaligned(image.add(0x3C).cast::<u32>()) as usize);
            ptr::read_unaligned(header.add(24 + 72).cast::<u64>()) as usize
        }
    }

    pub fn is_main_thread() -> bool {
        unsafe { GetCurrentThreadId() == MAIN_THREAD.load(Ordering::Relaxed) }
    }

    pub fn set_thread_name(name: &CStr) {
        let mut text = [0u16; 64];
        let n = unsafe { MultiByteToWideChar(CP_UTF8, 0, name.as_ptr(), -1, text.as_mut_ptr(), 64) };
        if n <= 0 {
            return;
        }
        // Windows 10 1607 and later; looked up, so older systems just go without.
        type SetDescription = unsafe extern "system" fn(Handle, *const u16) -> i32;
        let kernel = unsafe { GetModuleHandleW(wide(c"kernel32.dll").as_ptr()) };
        let set = unsafe { GetProcAddress(kernel, c"SetThreadDescription".as_ptr()) };
        if !set.is_null() {
            let set = unsafe { transmute::<*mut c_void, SetDescription>(set) };
            unsafe { set(GetCurrentThread(), text.as_ptr()) };
        }
    }

    pub fn lower_thread_priority() {
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
    }

    pub fn idle_thread_priority() {
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_IDLE) };
    }

    // No symbols without a PDB: each frame as its offset in the executable, which
    // `llvm-symbolizer --obj=mw2.exe` turns back into a name on the build machine.
    pub fn print_backtrace(why: &CStr) {
        let mut frames = [ptr::null_mut::<c_void>(); 64];
        let n = unsafe { RtlCaptureStackBackTrace(0, 64, frames.as_mut_ptr(), ptr::null_mut()) };
        let module = unsafe { GetModuleHandleW(ptr::null()) };
        let err = stream(2);
        unsafe { fprintf(err, c"\n[E] ---- %s (%u frames) ----\n".as_ptr(), why.as_ptr(), u32::from(n)) };
        for &frame in &frames[..usize::from(n)] {
            let mut owner = ptr::null_mut();
            let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
            unsafe { GetModuleHandleExW(flags, frame.cast(), &mut owner) };
            if owner == module {
                unsafe { fprintf(err, c"[E]   exe+0x%llx\n".as_ptr(), (frame as u64).wrapping_sub(module as u64)) };
            } else {
                unsafe { fprintf(err, c"[E]   %p\n".as_ptr(), frame) };
            }
        }
        unsafe {
            fprintf(err, c"[E] --------------------------------\n".as_ptr());
            fflush(err);
        }
    }

    pub fn processor_threads() -> u32 {
        std::thread::available_parallelism().map_or(0, |threads| threads.get() as u32)
    }

    pub fn thread_clock() -> usize {
        let mut own = ptr::null_mut();
        // A handle of its own: GetCurrentThread's means whichever thread asks.
        let process = unsafe { GetCurrentProcess() };
        let made = unsafe {
            DuplicateHandle(process, GetCurrentThread(), process, &mut own, THREAD_QUERY_LIMITED_INFORMATION, 0, 0)
        };
        if made == 0 { 0 } else { own as usize }
    }

    /// The calling thread's processor time so far, in nanoseconds.
    #[cfg(feature = "diagnostics")]
    pub fn thread_cpu_time() -> u64 {
        let (mut created, mut exited, mut kernel, mut user) = Default::default();
        unsafe { GetThreadTimes(GetCurrentThread(), &mut created, &mut exited, &mut kernel, &mut user) };
        nanoseconds(kernel).wrapping_add(nanoseconds(user))
    }

    pub fn thread_time(clock: usize) -> Option<u64> {
        let (mut created, mut exited, mut kernel, mut user) = Default::default();
        let got = clock != 0
            && unsafe { GetThreadTimes(clock as Handle, &mut created, &mut exited, &mut kernel, &mut user) } != 0;
        got.then(|| nanoseconds(kernel).wrapping_add(nanoseconds(user)))
    }

    pub fn processor_load(load: &mut Load) -> bool {
        let (mut created, mut exited, mut kernel, mut user, mut idle) = Default::default();
        if unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
            return false;
        }
        load.process = nanoseconds(kernel).wrapping_add(nanoseconds(user));
        // The system's kernel time has its idle time in it.
        if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
            return false;
        }
        load.total = nanoseconds(kernel).wrapping_add(nanoseconds(user));
        load.busy = load.total.wrapping_sub(nanoseconds(idle));
        true
    }

    pub fn memory_state(memory: &mut MemoryState) {
        let mut kilobytes = 0;
        if unsafe { GetPhysicallyInstalledSystemMemory(&mut kilobytes) } != 0 {
            memory.installed = kilobytes << 10;
        }
        let mut status = MemoryStatus { length: size_of::<MemoryStatus>() as u32, ..Default::default() };
        if unsafe { GlobalMemoryStatusEx(&mut status) } != 0 {
            memory.usable = status.total_physical;
            memory.free = status.available_physical;
            memory.commit_limit = status.total_page_file;
            memory.committed = status.total_page_file - status.available_page_file;
        }
        let mut counters =
            ProcessMemoryCounters { size: size_of::<ProcessMemoryCounters>() as u32, ..Default::default() };
        if unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.size) } != 0 {
            memory.process_peak = counters.peak_working_set as u64;
            memory.process_committed = counters.peak_page_file as u64;
        }
    }

    pub fn open_library(path: &CStr) -> *mut c_void {
        // The library's own folder is searched for what it loads in turn.
        unsafe { LoadLibraryExW(wide(path).as_ptr(), ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH) }
    }

    pub fn loaded_library(name: &CStr) -> *mut c_void {
        unsafe { GetModuleHandleA(name.as_ptr()) }
    }

    pub unsafe fn symbol(library: *mut c_void, name: &CStr) -> *mut c_void {
        unsafe { GetProcAddress(library, name.as_ptr()) }
    }

    /// What tells this machine from others, as its system names it; empty when it
    /// does not.
    pub fn machine_id() -> Vec<u8> {
        let mut text = [0u8; 64];
        let mut size = text.len() as u32 - 1;
        let found = unsafe {
            RegGetValueA(
                HKEY_LOCAL_MACHINE,
                c"SOFTWARE\\Microsoft\\Cryptography".as_ptr(),
                c"MachineGuid".as_ptr(),
                RRF_RT_REG_SZ,
                ptr::null_mut(),
                text.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if found != ERROR_SUCCESS {
            return Vec::new();
        }
        text.split(|&byte| byte == 0).next().unwrap_or_default().to_vec()
    }

    /// The hour and the minute on this machine's clock.
    pub fn local_hour_minute() -> (u32, u32) {
        let mut time = SystemTime::default();
        unsafe { GetLocalTime(&mut time) };
        (u32::from(time.hour), u32::from(time.minute))
    }

    /// A number from the system's random source.
    pub fn random() -> u64 {
        let mut value = 0u64;
        while unsafe { RtlGenRandom((&raw mut value).cast(), 8) } == 0 {}
        value
    }

    // C23's round-half-to-even, which the recompiled vector rounding reaches through
    // simde and the UCRT does not have. Whatever the rounding mode: a tie goes to the
    // even neighbour, anything else to the nearer one.
    #[unsafe(no_mangle)]
    extern "C" fn roundevenf(x: f32) -> f32 {
        let nearest = x.round(); // ties away from zero
        if (nearest - x).abs() != 0.5 {
            return nearest;
        }
        2.0 * (x * 0.5).round()
    }

    /// Makes [address, address + len) read-only, or readable and writable again: what
    /// the GPU's memory watch asks (gpu/memory_watch.rs). Safe in the vectored
    /// exception handler: VirtualProtect is one system call, and this allocates
    /// nothing, logs nothing and takes no lock.
    ///
    /// # Safety
    /// The range is page-aligned memory this process mapped, which nothing that
    /// cannot take the fault relies on being writable.
    #[cfg(feature = "rust-renderer")]
    #[inline]
    pub unsafe fn protect(address: *mut u8, len: usize, writable: bool) -> bool {
        let mut was = 0u32;
        let protection = if writable { PAGE_READWRITE } else { PAGE_READONLY };
        // SAFETY: the caller's contract; `was` is this frame's.
        unsafe { VirtualProtect(address.cast(), len, protection, &mut was) != 0 }
    }
}
