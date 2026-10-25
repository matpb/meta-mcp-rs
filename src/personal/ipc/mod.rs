//! The one place that knows how the CLI, the MCP tools and the daemon talk: a unix socket, or a
//! Windows named pipe. Everything else in `personal` uses the names exported here.

use std::io;
use std::path::PathBuf;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::{
    ClientStream, ENDPOINT_KIND, Identity, Listener, ServerStream, accept_error_is_fatal, acquire,
    connect, default_endpoint, own_identity, peer_identity, server_identity, split,
    systemd_passed_one_fd,
};
#[cfg(windows)]
pub use windows::{
    ClientStream, ENDPOINT_KIND, Identity, Listener, ServerStream, accept_error_is_fatal, acquire,
    connect, default_endpoint, own_identity, peer_identity, server_identity, split,
};

#[derive(Debug, thiserror::Error)]
pub enum ListenError {
    #[error("another personal daemon is already running on {0}")]
    AlreadyRunning(PathBuf),
    #[error("cannot listen: {0}")]
    Io(#[from] io::Error),
}
