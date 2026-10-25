//! Where the daemon accepts clients: a systemd-passed socket, or one it binds itself.

use std::io;
use std::os::fd::FromRawFd;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};

use tokio::net::UnixListener;

pub struct Listener {
    pub listener: UnixListener,
    /// Set when the daemon bound the socket itself and removes it on exit.
    pub bound_path: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum ListenError {
    #[error("another personal daemon is already running on {0}")]
    AlreadyRunning(PathBuf),
    #[error("cannot listen: {0}")]
    Io(#[from] io::Error),
}

/// True when an `accept` error means the listener itself is unusable (not a socket, not listening).
pub fn accept_error_is_fatal(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::ENOTSOCK | libc::EINVAL | libc::EBADF)
    )
}

/// True when `LISTEN_FDS` is `1` and `LISTEN_PID` is this process.
pub fn systemd_passed_one_fd(fds: Option<&str>, pid: Option<&str>) -> bool {
    fds == Some("1") && pid.and_then(|p| p.parse::<u32>().ok()) == Some(std::process::id())
}

/// True when fd 3 is open and is a socket; adopting anything else would close or misuse it.
#[cfg(target_os = "linux")]
fn fd3_is_socket() -> io::Result<bool> {
    Ok(std::fs::metadata("/proc/self/fd/3")?
        .file_type()
        .is_socket())
}

#[cfg(not(target_os = "linux"))]
fn fd3_is_socket() -> io::Result<bool> {
    Err(io::Error::other(
        "socket activation through LISTEN_FDS is only supported on Linux",
    ))
}

#[allow(unsafe_code)]
fn take_fd3() -> io::Result<std::os::unix::net::UnixListener> {
    match fd3_is_socket() {
        Ok(true) => {}
        Ok(false) => {
            return Err(io::Error::other(
                "LISTEN_FDS names fd 3 but it is not a socket",
            ));
        }
        Err(e) => {
            return Err(io::Error::new(
                e.kind(),
                format!("LISTEN_FDS names fd 3 but it cannot be inspected: {e}"),
            ));
        }
    }
    // SAFETY: LISTEN_PID and LISTEN_FDS=1 say systemd handed this process fd 3, and it is an open socket
    // that nothing else owns.
    Ok(unsafe { std::os::unix::net::UnixListener::from_raw_fd(3) })
}

/// Duplicates the inherited listener (std sets close-on-exec on the copy), so no sidecar inherits it.
fn adopt(original: std::os::unix::net::UnixListener) -> io::Result<UnixListener> {
    let copy = original.try_clone()?;
    drop(original);
    copy.set_nonblocking(true)?;
    UnixListener::from_std(copy)
}

/// Takes fd 3 from systemd, or binds `socket`.
pub fn acquire(socket: &Path) -> Result<Listener, ListenError> {
    let fds = std::env::var("LISTEN_FDS").ok();
    let pid = std::env::var("LISTEN_PID").ok();
    if systemd_passed_one_fd(fds.as_deref(), pid.as_deref()) {
        return Ok(Listener {
            listener: adopt(take_fd3()?)?,
            bound_path: None,
        });
    }
    if let Some(parent) = socket.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    remove_stale(socket)?;
    // The parent was just made 0700, so no other user can reach the socket before the chmod below.
    let listener = UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(Listener {
        listener,
        bound_path: Some(socket.to_path_buf()),
    })
}

/// A live socket means another daemon runs; a dead one is removed; anything else is refused.
fn remove_stale(path: &Path) -> Result<(), ListenError> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_socket() => {
            if std::os::unix::net::UnixStream::connect(path).is_ok() {
                return Err(ListenError::AlreadyRunning(path.to_path_buf()));
            }
            Ok(std::fs::remove_file(path)?)
        }
        Ok(_) => {
            Err(io::Error::other(format!("{} exists and is not a socket", path.display())).into())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_errors_that_mean_a_dead_listener_are_fatal() {
        for code in [libc::ENOTSOCK, libc::EINVAL, libc::EBADF] {
            assert!(accept_error_is_fatal(&io::Error::from_raw_os_error(code)));
        }
        for code in [libc::EMFILE, libc::ECONNABORTED, libc::EINTR] {
            assert!(!accept_error_is_fatal(&io::Error::from_raw_os_error(code)));
        }
        assert!(!accept_error_is_fatal(&io::Error::other("x")));
    }

    #[test]
    fn systemd_fd_needs_one_fd_and_our_pid() {
        let me = std::process::id().to_string();
        assert!(systemd_passed_one_fd(Some("1"), Some(&me)));
        assert!(!systemd_passed_one_fd(Some("2"), Some(&me)));
        assert!(!systemd_passed_one_fd(None, Some(&me)));
        assert!(!systemd_passed_one_fd(Some("1"), None));
        assert!(!systemd_passed_one_fd(Some("1"), Some("1")));
        assert!(!systemd_passed_one_fd(Some("1"), Some("x")));
    }

    #[tokio::test]
    async fn adopting_leaves_a_working_listener_on_a_new_close_on_exec_fd() {
        use std::os::fd::AsRawFd;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s");
        let original = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        let old_fd = original.as_raw_fd();
        let adopted = adopt(original).expect("adopt");
        assert_ne!(adopted.as_raw_fd(), old_fd);
        let _client = tokio::net::UnixStream::connect(&path)
            .await
            .expect("connect");
        let (_stream, _) = adopted.accept().await.expect("accept");
    }

    #[tokio::test]
    async fn bind_makes_a_private_socket_and_refuses_a_live_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sub/s.sock");
        let first = acquire(&path).expect("first bind");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let parent_mode = std::fs::metadata(path.parent().expect("parent"))
            .expect("stat")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(parent_mode, 0o700);
        let err = acquire(&path).err().expect("second bind fails");
        assert!(matches!(err, ListenError::AlreadyRunning(_)), "{err}");
        drop(first);
    }

    #[tokio::test]
    async fn a_stale_socket_is_replaced_but_a_regular_file_is_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).expect("bind"));
        assert!(path.exists());
        acquire(&path).expect("stale replaced");
        let file = dir.path().join("f");
        std::fs::write(&file, b"x").expect("write");
        assert!(acquire(&file).is_err());
        assert!(file.exists());
    }
}
