//! Personal Messenger and WhatsApp accounts: a long-lived daemon owns the Go sidecars and a
//! unix socket serves the CLI and the MCP tools.

pub mod audit;
pub mod cli;
pub mod client;
pub mod daemon;
pub mod link;
pub mod protocol;
pub mod sidecar;
pub mod state;
pub mod validate;

use std::path::{Path, PathBuf};

use crate::inbox::Platform;

pub const ENV_SOCKET: &str = "META_MCP_PERSONAL_SOCKET";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PersonalPlatform {
    Messenger,
    WhatsApp,
}

impl PersonalPlatform {
    pub const ALL: [Self; 2] = [Self::Messenger, Self::WhatsApp];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Messenger => "messenger",
            Self::WhatsApp => "whatsapp",
        }
    }

    /// Accepts the short names and the inbox names, case-insensitively.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "messenger" | "messenger_personal" => Some(Self::Messenger),
            "whatsapp" | "whatsapp_personal" => Some(Self::WhatsApp),
            _ => None,
        }
    }

    pub fn binary_name(self) -> &'static str {
        match self {
            Self::Messenger => "meta-messenger-sidecar",
            Self::WhatsApp => "meta-whatsapp-sidecar",
        }
    }

    pub fn inbox_platform(self) -> Platform {
        match self {
            Self::Messenger => Platform::MessengerPersonal,
            Self::WhatsApp => Platform::WhatsAppPersonal,
        }
    }
}

/// `META_MCP_PERSONAL_SOCKET`, else `$XDG_RUNTIME_DIR/meta-mcp/personal.sock`, else under the home.
pub fn default_socket_path() -> PathBuf {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    if let Some(p) = env(ENV_SOCKET) {
        return PathBuf::from(p);
    }
    if let Some(dir) = env("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("meta-mcp/personal.sock");
    }
    let home = env("META_MCP_HOME")
        .map(PathBuf::from)
        .or_else(|| crate::config::default_home().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    personal_dir(&home).join("personal.sock")
}

/// The uid this process runs as, read from the peer credentials of a socket pair with itself.
pub(crate) async fn own_uid() -> std::io::Result<u32> {
    let (a, _b) = tokio::net::UnixStream::pair()?;
    Ok(a.peer_cred()?.uid())
}

/// `<home>/personal`, where state, audit log and sidecar data live.
pub fn personal_dir(home: &Path) -> PathBuf {
    home.join("personal")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_names_round_trip_and_parse() {
        for p in PersonalPlatform::ALL {
            assert_eq!(PersonalPlatform::parse(p.as_str()), Some(p));
        }
        assert_eq!(
            PersonalPlatform::parse(" WhatsApp_Personal "),
            Some(PersonalPlatform::WhatsApp)
        );
        assert_eq!(
            PersonalPlatform::parse("messenger_personal"),
            Some(PersonalPlatform::Messenger)
        );
        assert_eq!(PersonalPlatform::parse("sms"), None);
        assert_eq!(
            PersonalPlatform::Messenger.binary_name(),
            "meta-messenger-sidecar"
        );
        assert_eq!(
            PersonalPlatform::WhatsApp.inbox_platform().as_str(),
            "whatsapp_personal"
        );
    }

    #[test]
    fn personal_dir_is_under_the_home() {
        assert_eq!(personal_dir(Path::new("/h")), PathBuf::from("/h/personal"));
    }
}
