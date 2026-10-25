//! The named pipe transport: `\\.\pipe\<name>`, owner-only, local clients only.

use std::ffi::OsString;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, PipeMode, ServerOptions,
};
use tokio::time::Instant;
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INVALID_HANDLE, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, HANDLE,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};

use super::ListenError;
use crate::personal::winsec::{self, OwnerOnly};

/// The user SID in string form (`S-1-5-21-...`).
pub type Identity = String;
pub type ServerStream = NamedPipeServer;
pub type ClientStream = NamedPipeClient;

/// Word for the endpoint in messages shown to users.
pub const ENDPOINT_KIND: &str = "pipe";

const PIPE_PREFIX: &str = r"\\.\pipe\";
const BUSY_RETRY: Duration = Duration::from_millis(50);
const BUSY_TOTAL: Duration = Duration::from_secs(2);

pub struct Listener {
    name: OsString,
    /// The default pipe DACL also grants read to Everyone and Anonymous; this one is owner-only.
    owner: OwnerOnly,
    /// The instance waiting for the next client.
    next: NamedPipeServer,
}

fn is_os_error(e: &io::Error, code: u32) -> bool {
    e.raw_os_error() == Some(code as i32)
}

#[allow(unsafe_code)]
fn create_instance(name: &OsString, owner: &OwnerOnly, first: bool) -> io::Result<NamedPipeServer> {
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .access_inbound(true)
        .access_outbound(true)
        .pipe_mode(PipeMode::Byte);
    // SAFETY: the pointer addresses a SECURITY_ATTRIBUTES owned by `owner`, which outlives the call.
    unsafe { options.create_with_security_attributes_raw(name, owner.as_security_attributes()) }
}

impl Listener {
    pub async fn accept(&mut self) -> io::Result<ServerStream> {
        match self.next.connect().await {
            Ok(()) => {}
            Err(e) if is_os_error(&e, ERROR_PIPE_CONNECTED) => {}
            Err(e) => {
                if let Ok(fresh) = create_instance(&self.name, &self.owner, false) {
                    self.next = fresh;
                }
                return Err(e);
            }
        }
        match create_instance(&self.name, &self.owner, false) {
            Ok(fresh) => Ok(std::mem::replace(&mut self.next, fresh)),
            Err(e) => {
                let _ = self.next.disconnect();
                Err(e)
            }
        }
    }

    /// Always false: a pipe is never handed over by a service manager.
    pub fn socket_activated(&self) -> bool {
        false
    }

    /// Nothing to remove: the pipe disappears with the last instance.
    pub fn cleanup(&self) {}
}

/// Creates the first pipe instance, or reports that another daemon already serves the name.
pub fn acquire(endpoint: &Path) -> Result<Listener, ListenError> {
    if !endpoint.to_string_lossy().starts_with(PIPE_PREFIX) {
        return Err(io::Error::other(format!(
            "{} is not a named pipe: a pipe name starts with {PIPE_PREFIX}",
            endpoint.display()
        ))
        .into());
    }
    let name = endpoint.as_os_str().to_owned();
    let owner = OwnerOnly::for_current_user()?;
    match create_instance(&name, &owner, true) {
        Ok(next) => Ok(Listener { name, owner, next }),
        Err(e) if is_os_error(&e, ERROR_ACCESS_DENIED) || is_os_error(&e, ERROR_PIPE_BUSY) => {
            match ClientOptions::new().open(&name) {
                Ok(_) => Err(ListenError::AlreadyRunning(endpoint.to_path_buf())),
                Err(open) if is_os_error(&open, ERROR_PIPE_BUSY) => {
                    Err(ListenError::AlreadyRunning(endpoint.to_path_buf()))
                }
                Err(_) => Err(e.into()),
            }
        }
        Err(e) => Err(e.into()),
    }
}

/// Only an invalid handle means the listener itself is unusable.
pub fn accept_error_is_fatal(e: &io::Error) -> bool {
    is_os_error(e, ERROR_INVALID_HANDLE)
}

/// The SID of the user this process runs as.
pub async fn own_identity() -> io::Result<Identity> {
    winsec::current_user_sid()
}

#[allow(unsafe_code)]
pub fn peer_identity(stream: &ServerStream) -> io::Result<Identity> {
    let mut pid = 0u32;
    // SAFETY: the handle is open for as long as `stream` is borrowed; `pid` is a plain local.
    let ok = unsafe { GetNamedPipeClientProcessId(stream.as_raw_handle() as HANDLE, &mut pid) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    winsec::process_user_sid(pid)
}

#[allow(unsafe_code)]
pub fn server_identity(stream: &ClientStream) -> io::Result<Identity> {
    let mut pid = 0u32;
    // SAFETY: the handle is open for as long as `stream` is borrowed; `pid` is a plain local.
    let ok = unsafe { GetNamedPipeServerProcessId(stream.as_raw_handle() as HANDLE, &mut pid) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    winsec::process_user_sid(pid)
}

/// Splits a client stream; the pipe closes when the read half is dropped too.
pub fn split(
    stream: ClientStream,
) -> (
    tokio::io::ReadHalf<ClientStream>,
    tokio::io::WriteHalf<ClientStream>,
) {
    tokio::io::split(stream)
}

/// Opens the pipe, waiting up to 2 s while every instance is busy.
pub async fn connect(endpoint: &Path) -> io::Result<ClientStream> {
    let deadline = Instant::now() + BUSY_TOTAL;
    loop {
        match ClientOptions::new().open(endpoint) {
            Err(e) if is_os_error(&e, ERROR_PIPE_BUSY) && Instant::now() < deadline => {
                tokio::time::sleep(BUSY_RETRY).await;
            }
            other => return other,
        }
    }
}

/// `%USERNAME%` reduced to `[A-Za-z0-9._-]`, `user` when empty.
fn sanitize_username(raw: &str) -> String {
    let clean: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if clean.is_empty() {
        "user".to_owned()
    } else {
        clean
    }
}

/// `META_MCP_PERSONAL_SOCKET`, else `\\.\pipe\meta-mcp-personal-<USERNAME>`.
pub fn default_endpoint() -> PathBuf {
    if let Some(p) = std::env::var_os(crate::personal::ENV_SOCKET).filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    let user = std::env::var("USERNAME").unwrap_or_default();
    PathBuf::from(format!(
        "{PIPE_PREFIX}meta-mcp-personal-{}",
        sanitize_username(&user)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    fn test_pipe() -> PathBuf {
        PathBuf::from(format!(
            "{PIPE_PREFIX}meta-mcp-test-{}",
            crate::config::secret::random_hex(8)
        ))
    }

    #[test]
    fn usernames_are_reduced_to_safe_characters() {
        assert_eq!(sanitize_username("Jane Doe"), "Jane_Doe");
        assert_eq!(sanitize_username("a.b-c_d"), "a.b-c_d");
        assert_eq!(sanitize_username("x\\y/z"), "x_y_z");
        assert_eq!(sanitize_username(""), "user");
    }

    #[test]
    fn a_name_without_the_pipe_prefix_is_refused() {
        let err = acquire(Path::new(r"C:\temp\x")).err().expect("refused");
        assert!(matches!(err, ListenError::Io(_)), "{err}");
    }

    #[tokio::test]
    async fn a_client_round_trips_a_line_and_both_ends_see_our_sid() {
        let name = test_pipe();
        let mut listener = acquire(&name).expect("acquire");
        assert!(!listener.socket_activated());
        let own = own_identity().await.expect("sid");
        let client_name = name.clone();
        let client = tokio::spawn(async move {
            let stream = connect(&client_name).await.expect("connect");
            assert_eq!(
                server_identity(&stream).expect("server sid"),
                own_identity().await.expect("sid")
            );
            let (_read, mut write) = tokio::io::split(stream);
            write.write_all(b"hello\n").await.expect("write");
            write.flush().await.expect("flush");
        });
        let stream = listener.accept().await.expect("accept");
        assert_eq!(peer_identity(&stream).expect("peer sid"), own);
        let (read, _write) = tokio::io::split(stream);
        let mut line = String::new();
        BufReader::new(read)
            .read_line(&mut line)
            .await
            .expect("read");
        assert_eq!(line, "hello\n");
        client.await.expect("client task");

        let err = acquire(&name).err().expect("second acquire fails");
        assert!(matches!(err, ListenError::AlreadyRunning(_)), "{err}");
        listener.cleanup();
    }

    #[tokio::test]
    async fn a_missing_pipe_is_not_found() {
        let err = connect(&test_pipe()).await.expect_err("no server");
        assert_eq!(err.kind(), io::ErrorKind::NotFound, "{err}");
    }
}
