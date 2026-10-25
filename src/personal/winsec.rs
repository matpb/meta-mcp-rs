//! Windows security helpers: user SIDs, owner-only security descriptors and ACLs, and the daemon's
//! log file when it runs without a console.

use std::ffi::{OsStr, c_void};
use std::fs::OpenOptions;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::IntoRawHandle;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation,
    PROTECTED_DACL_SECURITY_INFORMATION, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::System::Console::{
    FreeConsole, GetConsoleProcessList, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
};
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CreateProcessW,
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, STARTUPINFOW,
};

const LOG_ROTATE_BYTES: u64 = 10 * 1024 * 1024;

/// Closes the wrapped handle on drop.
struct Handle(HANDLE);

impl Drop for Handle {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle came from OpenProcess or OpenProcessToken and is closed once, here.
            unsafe { CloseHandle(self.0) };
        }
    }
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// The text of a NUL-terminated UTF-16 string.
#[allow(unsafe_code)]
fn string_from_wide_ptr(p: *const u16) -> String {
    let mut len = 0;
    // SAFETY: callers pass a NUL-terminated string returned by the OS.
    while unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: the `len` units before the terminator were just read.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) })
}

#[allow(unsafe_code)]
fn sid_to_string(sid: PSID) -> io::Result<String> {
    let mut out: *mut u16 = null_mut();
    // SAFETY: `sid` is a valid SID; on success `out` is a LocalAlloc'd NUL-terminated string.
    if unsafe { ConvertSidToStringSidW(sid, &mut out) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let text = string_from_wide_ptr(out);
    // SAFETY: `out` was allocated by ConvertSidToStringSidW and is freed once.
    unsafe { LocalFree(out.cast()) };
    Ok(text)
}

#[allow(unsafe_code)]
fn token_user_sid(token: HANDLE) -> io::Result<String> {
    let mut needed = 0u32;
    // SAFETY: a null buffer of length 0 only asks for the size.
    unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64 words keep the buffer aligned for TOKEN_USER.
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: `buf` holds at least `needed` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success the buffer starts with a TOKEN_USER whose SID lives inside the buffer.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    sid_to_string(sid)
}

#[allow(unsafe_code)]
fn sid_of_process_handle(process: HANDLE) -> io::Result<String> {
    let mut token: HANDLE = null_mut();
    // SAFETY: `process` is a valid process handle and `token` receives a new handle.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Handle(token);
    token_user_sid(token.0)
}

/// The user SID of this process, in string form (`S-1-5-21-...`).
#[allow(unsafe_code)]
pub fn current_user_sid() -> io::Result<String> {
    // SAFETY: GetCurrentProcess returns a pseudo handle that needs no closing.
    sid_of_process_handle(unsafe { GetCurrentProcess() })
}

/// The user SID of the process `pid`; fails when it exited or cannot be queried.
#[allow(unsafe_code)]
pub fn process_user_sid(pid: u32) -> io::Result<String> {
    // SAFETY: plain call; a null result is checked below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let process = Handle(process);
    sid_of_process_handle(process.0)
}

/// A security descriptor built from SDDL, freed on drop.
pub struct OwnerOnly {
    sd: *mut c_void,
    attrs: Box<SECURITY_ATTRIBUTES>,
}

// SAFETY: the descriptor is immutable after construction and only read by the OS.
#[allow(unsafe_code)]
unsafe impl Send for OwnerOnly {}
// SAFETY: see Send.
#[allow(unsafe_code)]
unsafe impl Sync for OwnerOnly {}

impl OwnerOnly {
    #[allow(unsafe_code)]
    pub fn from_sddl(sddl: &str) -> io::Result<Self> {
        let text = wide(OsStr::new(sddl));
        let mut sd: *mut c_void = null_mut();
        // SAFETY: `text` is NUL-terminated; on success `sd` is a LocalAlloc'd descriptor.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let attrs = Box::new(SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        });
        Ok(Self { sd, attrs })
    }

    /// Full control for the current user and nobody else, with inheritance blocked.
    pub fn for_current_user() -> io::Result<Self> {
        Self::from_sddl(&format!("D:P(A;;GA;;;{})", current_user_sid()?))
    }

    /// The SECURITY_ATTRIBUTES to pass to a create call; valid while `self` lives.
    pub fn as_security_attributes(&self) -> *mut c_void {
        (&raw const *self.attrs).cast_mut().cast()
    }
}

impl Drop for OwnerOnly {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: `sd` was allocated by the OS in `from_sddl` and is freed once.
        unsafe { LocalFree(self.sd) };
    }
}

/// Replaces the DACL of `path` with one full-control ACE for the current user's SID, no inheritance
/// (children of a folder inherit it); OW/CO would resolve to BUILTIN\Administrators for an admin.
#[allow(unsafe_code)]
pub fn restrict_to_owner(path: &Path, dir: bool) -> io::Result<()> {
    let sid = current_user_sid()?;
    let ace = if dir { "OICI" } else { "" };
    let owner = OwnerOnly::from_sddl(&format!("D:P(A;{ace};FA;;;{sid})"))?;
    let (mut present, mut defaulted) = (0i32, 0i32);
    let mut dacl: *mut ACL = null_mut();
    // SAFETY: `owner.sd` is a valid descriptor; the outputs are plain locals.
    let ok =
        unsafe { GetSecurityDescriptorDacl(owner.sd, &mut present, &mut dacl, &mut defaulted) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if present == 0 || dacl.is_null() {
        return Err(io::Error::other("the owner-only descriptor has no DACL"));
    }
    let name = wide(path.as_os_str());
    // SAFETY: `name` is NUL-terminated and `dacl` points into `owner`, which outlives the call.
    let code = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl,
            null(),
        )
    };
    if code != 0 {
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    Ok(())
}

fn log_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty())?;
    Some(PathBuf::from(base).join("meta-mcp").join("logs"))
}

fn open_log() -> io::Result<std::fs::File> {
    let dir = log_dir().ok_or_else(|| io::Error::other("LOCALAPPDATA is not set"))?;
    super::secure::private_dir(&dir)?;
    let log = dir.join("daemon.log");
    if std::fs::metadata(&log).is_ok_and(|m| m.len() > LOG_ROTATE_BYTES) {
        let _ = std::fs::rename(&log, dir.join("daemon.log.1"));
    }
    OpenOptions::new().create(true).append(true).open(&log)
}

/// When the process owns its console alone or has none (Task Scheduler), drops the console and
/// sends stdout and stderr to `%LOCALAPPDATA%\meta-mcp\logs\daemon.log`. Failures are ignored.
#[allow(unsafe_code)]
pub fn detach_console_to_log() {
    let mut attached = [0u32; 2];
    // SAFETY: the buffer holds the 2 entries the call is told about.
    let count = unsafe { GetConsoleProcessList(attached.as_mut_ptr(), 2) };
    if count >= 2 {
        return;
    }
    let Ok(file) = open_log() else {
        return;
    };
    // Leaked on purpose: the std handles must stay valid for the life of the process.
    let handle: HANDLE = file.into_raw_handle();
    // SAFETY: `handle` is an open, never-closed file handle.
    unsafe {
        FreeConsole();
        SetStdHandle(STD_OUTPUT_HANDLE, handle);
        SetStdHandle(STD_ERROR_HANDLE, handle);
    }
}

/// Starts `exe args` with no window, no inherited handles and outside the caller's job (when the
/// job allows it), so neither an SSH session nor a captured shell waits for or kills it.
pub fn spawn_detached(exe: &Path, args: &str) -> io::Result<u32> {
    let base = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
    match create_process(exe, args, base | CREATE_BREAKAWAY_FROM_JOB) {
        Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
            create_process(exe, args, base)
        }
        other => other,
    }
}

#[allow(unsafe_code)]
fn create_process(exe: &Path, args: &str, flags: u32) -> io::Result<u32> {
    let app = wide(exe.as_os_str());
    let mut line = wide(OsStr::new(&format!("\"{}\" {args}", exe.display())));
    // SAFETY: STARTUPINFOW and PROCESS_INFORMATION are plain C structs for which zero is valid.
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: both strings are NUL-terminated and live across the call; the command line buffer
    // is writable as CreateProcessW requires; handles are not inherited.
    let ok = unsafe {
        CreateProcessW(
            app.as_ptr(),
            line.as_mut_ptr(),
            null(),
            null(),
            0,
            flags,
            null(),
            null(),
            &startup,
            &mut info,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both handles were just returned by CreateProcessW and are closed once.
    unsafe {
        CloseHandle(info.hThread);
        CloseHandle(info.hProcess);
    }
    Ok(info.dwProcessId)
}

/// The SDDL of the DACL of `path`, for tests.
#[cfg(test)]
#[allow(unsafe_code)]
pub fn dacl_sddl(path: &Path) -> io::Result<String> {
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
    };
    let name = wide(path.as_os_str());
    let mut sd: *mut c_void = null_mut();
    // SAFETY: `name` is NUL-terminated; on success `sd` is a LocalAlloc'd descriptor.
    let code = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sd,
        )
    };
    if code != 0 {
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    let mut text: *mut u16 = null_mut();
    // SAFETY: `sd` is valid; on success `text` is a LocalAlloc'd string.
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            null_mut(),
        )
    };
    let result = if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        let s = string_from_wide_ptr(text);
        // SAFETY: `text` was allocated by the call above.
        unsafe { LocalFree(text.cast()) };
        Ok(s)
    };
    // SAFETY: `sd` was allocated by GetNamedSecurityInfoW.
    unsafe { LocalFree(sd) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_user_sid_looks_like_a_sid_and_is_the_same_for_our_pid() {
        let sid = current_user_sid().expect("sid");
        assert!(sid.starts_with("S-1-"), "{sid}");
        assert_eq!(process_user_sid(std::process::id()).expect("by pid"), sid);
    }

    #[test]
    fn a_private_folder_has_one_protected_ace_for_the_user() {
        let tmp = tempfile::tempdir().expect("tempdir");
        restrict_to_owner(tmp.path(), true).expect("restrict");
        let sid = current_user_sid().expect("sid");
        let sddl = dacl_sddl(tmp.path()).expect("sddl");
        assert!(sddl.starts_with("D:P"), "{sddl}");
        assert_eq!(sddl.matches('(').count(), 1, "{sddl}");
        assert!(sddl.contains(&format!("(A;OICI;FA;;;{sid})")), "{sddl}");
    }

    #[test]
    fn a_private_file_has_one_protected_ace_for_the_user_and_stays_usable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("f");
        std::fs::write(&path, b"x").expect("write");
        restrict_to_owner(&path, false).expect("restrict");
        let sid = current_user_sid().expect("sid");
        let sddl = dacl_sddl(&path).expect("sddl");
        assert!(sddl.starts_with("D:P"), "{sddl}");
        assert_eq!(sddl.matches('(').count(), 1, "{sddl}");
        assert!(sddl.contains(&format!("(A;;FA;;;{sid})")), "{sddl}");
        assert_eq!(std::fs::read(&path).expect("read"), b"x");
    }
}
