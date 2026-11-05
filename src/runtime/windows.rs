// What the runtime asks of Windows, declared by hand as the C++ had it from
// windows.h: nothing here has any logic.
#![allow(non_snake_case)]

use std::ffi::{CStr, c_char, c_int, c_void};

pub type Handle = *mut c_void;
pub type Module = *mut c_void;

pub const EXCEPTION_ACCESS_VIOLATION: u32 = 0xC0000005;
pub const EXCEPTION_ILLEGAL_INSTRUCTION: u32 = 0xC000001D;
pub const EXCEPTION_INT_DIVIDE_BY_ZERO: u32 = 0xC0000094;
pub const EXCEPTION_BREAKPOINT: u32 = 0x80000003;
pub const EXCEPTION_STACK_OVERFLOW: u32 = 0xC00000FD;
pub const EXCEPTION_IN_PAGE_ERROR: u32 = 0xC0000006;
pub const EXCEPTION_CONTINUE_EXECUTION: i32 = -1;
pub const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

#[repr(C)]
pub struct ExceptionRecord {
    pub code: u32,
    pub flags: u32,
    pub record: *mut ExceptionRecord,
    pub address: *mut c_void,
    pub parameters: u32,
    pub information: [usize; 15],
}

#[repr(C)]
pub struct ExceptionPointers {
    pub record: *mut ExceptionRecord,
    pub context: *mut c_void,
}

pub type ExceptionHandler = unsafe extern "system" fn(*mut ExceptionPointers) -> i32;

pub const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
pub const PAGE_NOACCESS: u32 = 0x01;
pub const PAGE_READWRITE: u32 = 0x04;
pub const MEM_COMMIT: u32 = 0x1000;
pub const MEM_RESERVE: u32 = 0x2000;
pub const MEM_RELEASE: u32 = 0x8000;
pub const MEM_REPLACE_PLACEHOLDER: u32 = 0x4000;
pub const MEM_RESERVE_PLACEHOLDER: u32 = 0x40000;
pub const MEM_PRESERVE_PLACEHOLDER: u32 = 0x02;
pub const CP_UTF8: u32 = 65001;
pub const CP_ACP: u32 = 0;
pub const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
pub const THREAD_PRIORITY_BELOW_NORMAL: c_int = -1;
pub const THREAD_PRIORITY_IDLE: c_int = -15;
pub const THREAD_QUERY_LIMITED_INFORMATION: u32 = 0x0800;
pub const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 0x04;
pub const GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT: u32 = 0x02;
pub const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x08;

#[repr(C)]
#[derive(Default)]
pub struct MemoryBasicInformation {
    pub base: usize,
    pub allocation_base: usize,
    pub allocation_protect: u32,
    pub partition: u16,
    pub size: usize,
    pub state: u32,
    pub protect: u32,
    pub kind: u32,
}

#[repr(C)]
#[derive(Default, Copy, Clone)]
pub struct FileTime {
    pub low: u32,
    pub high: u32,
}

#[repr(C)]
#[derive(Default)]
pub struct MemoryStatus {
    pub length: u32,
    pub load: u32,
    pub total_physical: u64,
    pub available_physical: u64,
    pub total_page_file: u64,
    pub available_page_file: u64,
    pub total_virtual: u64,
    pub available_virtual: u64,
    pub available_extended_virtual: u64,
}

#[repr(C)]
#[derive(Default)]
pub struct ProcessMemoryCounters {
    pub size: u32,
    pub page_fault_count: u32,
    pub peak_working_set: usize,
    pub working_set: usize,
    pub quota_peak_paged_pool: usize,
    pub quota_paged_pool: usize,
    pub quota_peak_non_paged_pool: usize,
    pub quota_non_paged_pool: usize,
    pub page_file: usize,
    pub peak_page_file: usize,
}

unsafe extern "system" {
    pub fn GetLastError() -> u32;
    pub fn GetModuleHandleW(name: *const u16) -> Module;
    pub fn GetModuleHandleA(name: *const c_char) -> Module;
    pub fn GetModuleHandleExW(flags: u32, name: *const u16, module: *mut Module) -> c_int;
    pub fn GetProcAddress(module: Module, name: *const c_char) -> *mut c_void;
    pub fn LoadLibraryExW(path: *const u16, file: Handle, flags: u32) -> Module;
    pub fn VirtualAlloc(address: *mut c_void, size: usize, kind: u32, protection: u32) -> *mut c_void;
    pub fn VirtualFree(address: *mut c_void, size: usize, kind: u32) -> c_int;
    pub fn VirtualQuery(address: *const c_void, info: *mut MemoryBasicInformation, size: usize) -> usize;
    pub fn CreateFileMappingW(
        file: Handle,
        attributes: *mut c_void,
        protection: u32,
        high: u32,
        low: u32,
        name: *const u16,
    ) -> Handle;
    pub fn CloseHandle(handle: Handle) -> c_int;
    pub fn AddVectoredExceptionHandler(first: u32, handler: ExceptionHandler) -> *mut c_void;
    pub fn TerminateProcess(process: Handle, code: u32) -> c_int;
    pub fn GetCurrentProcess() -> Handle;
    pub fn GetCurrentThread() -> Handle;
    pub fn GetCurrentThreadId() -> u32;
    pub fn SetThreadPriority(thread: Handle, priority: c_int) -> c_int;
    pub fn DuplicateHandle(
        from: Handle,
        source: Handle,
        to: Handle,
        target: *mut Handle,
        access: u32,
        inherit: c_int,
        options: u32,
    ) -> c_int;
    pub fn GetThreadTimes(
        thread: Handle,
        created: *mut FileTime,
        exited: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> c_int;
    pub fn GetProcessTimes(
        process: Handle,
        created: *mut FileTime,
        exited: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> c_int;
    pub fn GetSystemTimes(idle: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> c_int;
    pub fn GetConsoleWindow() -> *mut c_void;
    pub fn AttachConsole(process: u32) -> c_int;
    pub fn MultiByteToWideChar(
        page: u32,
        flags: u32,
        text: *const c_char,
        size: c_int,
        wide: *mut u16,
        room: c_int,
    ) -> c_int;
    pub fn RtlCaptureStackBackTrace(skip: u32, count: u32, frames: *mut *mut c_void, hash: *mut u32) -> u16;
    pub fn GetPhysicallyInstalledSystemMemory(kilobytes: *mut u64) -> c_int;
    pub fn GlobalMemoryStatusEx(status: *mut MemoryStatus) -> c_int;
    pub fn K32GetProcessMemoryInfo(process: Handle, counters: *mut ProcessMemoryCounters, size: u32) -> c_int;
    pub fn timeBeginPeriod(period: u32) -> u32;
    pub fn GetLocalTime(time: *mut SystemTime);
    pub fn RegGetValueA(
        key: Handle,
        subkey: *const c_char,
        value: *const c_char,
        flags: u32,
        kind: *mut u32,
        data: *mut c_void,
        size: *mut u32,
    ) -> i32;
    pub fn RegGetValueW(
        key: Handle,
        subkey: *const u16,
        value: *const u16,
        flags: u32,
        kind: *mut u32,
        data: *mut c_void,
        size: *mut u32,
    ) -> i32;
    #[link_name = "SystemFunction036"]
    pub fn RtlGenRandom(buffer: *mut c_void, size: u32) -> u8;
    pub fn SetEnvironmentVariableA(name: *const c_char, value: *const c_char) -> c_int;
    pub fn GetEnvironmentStringsA() -> *mut c_char;
    pub fn FreeEnvironmentStringsA(block: *mut c_char) -> c_int;
    pub fn WideCharToMultiByte(
        page: u32,
        flags: u32,
        wide: *const u16,
        size: c_int,
        text: *mut c_char,
        room: c_int,
        default: *const c_char,
        used: *mut c_int,
    ) -> c_int;
}

#[repr(C)]
#[derive(Default)]
pub struct SystemTime {
    pub year: u16,
    pub month: u16,
    pub day_of_week: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub milliseconds: u16,
}

// Sign-extended, as the SDK has them on x64.
pub const HKEY_CURRENT_USER: Handle = 0x8000_0001u32 as i32 as isize as Handle;
pub const HKEY_LOCAL_MACHINE: Handle = 0x8000_0002u32 as i32 as isize as Handle;
pub const RRF_RT_REG_SZ: u32 = 0x0000_0002;
pub const ERROR_SUCCESS: i32 = 0;

/// UTF-16 and its zero, of UTF-8 or ASCII.
pub fn wide(text: &CStr) -> Vec<u16> {
    wide_in(CP_UTF8, text)
}

/// UTF-16 and its zero, of text in `code_page`.
pub fn wide_in(code_page: u32, text: &CStr) -> Vec<u16> {
    let size = unsafe { MultiByteToWideChar(code_page, 0, text.as_ptr(), -1, std::ptr::null_mut(), 0) };
    let mut out = vec![0u16; size.max(1) as usize];
    unsafe { MultiByteToWideChar(code_page, 0, text.as_ptr(), -1, out.as_mut_ptr(), size) };
    out
}
