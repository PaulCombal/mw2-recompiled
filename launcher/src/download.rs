// One file over HTTPS, with what the system already has: Windows' own URL download
// (urlmon's URLDownloadToFileW), and elsewhere the curl program, or wget where curl is
// not installed (Debian and Ubuntu install wget and not curl; Fedora, Arch and openSUSE
// the other way round). The launcher
// fetches little -- the title update's package, a release's description and archive --
// and an HTTP and TLS client of its own would be far more code than these few files are
// worth, and one more thing to keep secure.
//
// The install (setup.rs) and the self-update (update.rs) call it.
#![deny(unsafe_code)]

use std::fs;
use std::path::Path;

/// Blocks until `url` is in `to`, or says why it is not. What `to` held is removed
/// first, so a file there afterwards is this download's; a failed download leaves none
/// (wget writes the file it was given even when the server refuses).
pub fn download(url: &str, to: &Path) -> Result<(), String> {
    let _ = fs::remove_file(to);
    if let Err(why) = fetch(url, to) {
        let _ = fs::remove_file(to);
        return Err(why);
    }
    if !to.is_file() {
        return Err("nothing was downloaded".into());
    }
    Ok(())
}

// The program's own errors on stderr, which a terminal shows; the launcher's say only
// that it failed, and its exit code, which the program's manual explains. wget is
// asked for what curl does here: a refusal is a failure, one try, no progress bar but
// its errors, and no waiting on a stalled connection for long (it has no limit on the
// whole transfer, as curl's --max-time is, only on each wait). Neither curl's file://
// nor its exit codes are wget's: what MW2_UPDATE_URL names for a test needs curl.
#[cfg(not(windows))]
fn fetch(url: &str, to: &Path) -> Result<(), String> {
    use std::io::{ErrorKind, Write};
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};

    let mut curl = Command::new("curl");
    curl.args(["--location", "--fail", "--silent", "--show-error", "--max-time", "300", "--output"]).arg(to).arg(url);
    let mut wget = Command::new("wget");
    wget.args(["--no-verbose", "--tries=1", "--timeout=60", "--output-document"]).arg(to).arg(url);

    let (program, status) = match curl.status() {
        Ok(status) => ("curl", status),
        // wget has no quiet mode that keeps its errors, so what it says is held and
        // shown only when it fails: a success line would land in the middle of the
        // terminal's progress line.
        Err(curl_error) => match wget.stderr(Stdio::piped()).output() {
            Ok(output) => {
                if !output.status.success() {
                    let _ = std::io::stderr().write_all(&output.stderr);
                }
                ("wget", output.status)
            }
            Err(wget_error) if curl_error.kind() == ErrorKind::NotFound && wget_error.kind() == ErrorKind::NotFound => {
                return Err("it needs the curl or the wget program, and neither is installed".into());
            }
            Err(_) => return Err("neither curl nor wget could be started".into()),
        },
    };
    // As SDL's process wait reports one a signal ended: the signal's number, negated.
    let code = status.code().or(status.signal().map(|signal| -signal)).unwrap_or(-1);
    if code != 0 {
        return Err(format!("{program} failed (exit code {code})"));
    }
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn fetch(url: &str, to: &Path) -> Result<(), String> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "urlmon")]
    unsafe extern "system" {
        fn URLDownloadToFileW(
            caller: *mut c_void,
            url: *const u16,
            file: *const u16,
            reserved: u32,
            callback: *mut c_void,
        ) -> i32;
    }

    let url: Vec<u16> = url.encode_utf16().chain([0]).collect();
    let file: Vec<u16> = to.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: two NUL-terminated wide strings, alive across the call; no caller object
    // and no status callback.
    let result = unsafe { URLDownloadToFileW(std::ptr::null_mut(), url.as_ptr(), file.as_ptr(), 0, std::ptr::null_mut()) };
    // An HRESULT: negative is a failure, given in hexadecimal as Windows documents them.
    if result < 0 {
        return Err(format!("the download failed ({:08X})", result as u32));
    }
    Ok(())
}
