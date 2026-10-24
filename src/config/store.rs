//! `credentials.json`: the secrets written by setup, stored atomically with private permissions.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use serde::{Deserialize, Serialize};

use super::ConfigError;
use super::secret::{Secret, redact};

pub const CREDENTIALS_FILE: &str = "credentials.json";
pub const CREDENTIALS_VERSION: u32 = 1;

static TMP_COUNTER: AtomicU32 = AtomicU32::new(0);

/// A user token with the metadata Meta reported when it was issued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredToken {
    pub token: Secret,
    #[serde(default)]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub obtained_at: i64,
}

/// A Facebook Page and its access token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageConfig {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub token: Secret,
}

/// Who issued the WhatsApp token; system user tokens do not expire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenKind {
    #[default]
    User,
    SystemUser,
}

/// A WhatsApp business phone number; the descriptive fields are empty when unknown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhoneNumber {
    pub id: String,
    #[serde(default)]
    pub display_phone_number: String,
    #[serde(default)]
    pub verified_name: String,
}

/// WhatsApp Cloud API credentials and the numbers they can send from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhatsAppConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waba_id: Option<String>,
    pub token: Secret,
    #[serde(default)]
    pub token_kind: TokenKind,
    #[serde(default)]
    pub phone_numbers: Vec<PhoneNumber>,
}

/// Webhook settings kept next to the credentials.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredWebhook {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_token: Option<Secret>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
}

/// The whole `credentials.json` document (version 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_secret: Option<Secret>,
    #[serde(default)]
    pub user_token: Option<StoredToken>,
    #[serde(default)]
    pub pages: Vec<PageConfig>,
    #[serde(default)]
    pub whatsapp: Option<WhatsAppConfig>,
    #[serde(default)]
    pub webhook: StoredWebhook,
}

fn default_version() -> u32 {
    CREDENTIALS_VERSION
}

impl Default for Credentials {
    fn default() -> Self {
        Self {
            version: CREDENTIALS_VERSION,
            app_id: None,
            app_secret: None,
            user_token: None,
            pages: Vec::new(),
            whatsapp: None,
            webhook: StoredWebhook::default(),
        }
    }
}

fn io_err(what: &str, path: &Path, e: &std::io::Error) -> ConfigError {
    ConfigError(format!("{what} {}: {e}", path.display()))
}

/// `<home>/credentials.json`.
pub fn credentials_path(home: &Path) -> PathBuf {
    home.join(CREDENTIALS_FILE)
}

/// Reads the credentials, or `None` when the file does not exist.
pub fn load(home: &Path) -> Result<Option<Credentials>, ConfigError> {
    let path = credentials_path(home);
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err("cannot read", &path, &e)),
    };
    warn_if_shared(&path);
    let creds: Credentials = serde_json::from_slice(&raw).map_err(|e| {
        ConfigError(format!(
            "{} is not valid credentials JSON ({}); fix it or run `meta-mcp setup` again",
            path.display(),
            redact(&e.to_string())
        ))
    })?;
    if creds.version > CREDENTIALS_VERSION {
        return Err(ConfigError(format!(
            "{} has version {}, newer than this build supports ({CREDENTIALS_VERSION}); upgrade meta-mcp",
            path.display(),
            creds.version
        )));
    }
    Ok(Some(creds))
}

#[cfg(unix)]
fn warn_if_shared(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            tracing::warn!(
                "{} is accessible by other users (mode {mode:o}): run `chmod 600` on it",
                path.display()
            );
        }
    }
}

#[cfg(not(unix))]
fn warn_if_shared(_path: &Path) {}

/// Creates `dir` (and parents) as 0700 on unix; an existing directory is tightened to 0700.
pub fn ensure_private_dir(dir: &Path) -> Result<(), ConfigError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| io_err("cannot create", dir, &e))?;
        if let Ok(meta) = std::fs::metadata(dir)
            && meta.permissions().mode() & 0o077 != 0
        {
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir).map_err(|e| io_err("cannot create", dir, &e))
    }
}

fn create_private_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

/// Writes the credentials atomically: private temp file, fsync, rename over the target.
pub fn save(home: &Path, creds: &Credentials) -> Result<(), ConfigError> {
    ensure_private_dir(home)?;
    let target = credentials_path(home);
    let mut json = serde_json::to_vec_pretty(creds)
        .map_err(|e| ConfigError(format!("cannot serialize credentials: {e}")))?;
    json.push(b'\n');
    let tmp = home.join(format!(
        ".{CREDENTIALS_FILE}.{}.{}.tmp",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let written = create_private_file(&tmp).and_then(|mut f| {
        f.write_all(&json)?;
        f.sync_all()
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_err("cannot write", &tmp, &e));
    }
    if let Err(e) = std::fs::rename(&tmp, &target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_err("cannot replace", &target, &e));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Credentials {
        Credentials {
            app_id: Some("123".into()),
            app_secret: Some(Secret::new("shh-secret")),
            user_token: Some(StoredToken {
                token: Secret::new("EAAuser"),
                expires_at: Some(1_900_000_000),
                scopes: vec!["pages_messaging".into()],
                obtained_at: 1_700_000_000,
            }),
            pages: vec![PageConfig {
                id: "42".into(),
                name: "My Page".into(),
                token: Secret::new("EAApage"),
            }],
            whatsapp: Some(WhatsAppConfig {
                waba_id: Some("w1".into()),
                token: Secret::new("EAAwa"),
                token_kind: TokenKind::SystemUser,
                phone_numbers: vec![PhoneNumber {
                    id: "p1".into(),
                    display_phone_number: "+1 555 0100".into(),
                    verified_name: "Shop".into(),
                }],
            }),
            webhook: StoredWebhook {
                verify_token: Some(Secret::new("vt")),
                public_url: Some("https://hooks.example.org/webhook".into()),
            },
            ..Credentials::default()
        }
    }

    #[test]
    fn missing_file_is_none() {
        let dir = tempfile::tempdir().expect("tmp");
        assert!(load(dir.path()).expect("load").is_none());
    }

    #[test]
    fn credentials_file_is_0600_in_0700_dir_and_rereads() {
        let dir = tempfile::tempdir().expect("tmp");
        let home = dir.path().join("nested").join("meta-mcp");
        let creds = sample();
        save(&home, &creds).expect("save");
        assert_eq!(load(&home).expect("load"), Some(creds.clone()));
        let on_disk = std::fs::read_to_string(credentials_path(&home)).expect("read");
        assert!(on_disk.contains("\"version\": 1"), "{on_disk}");
        assert!(
            on_disk.contains("\"token_kind\": \"system_user\""),
            "{on_disk}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode(&home), 0o700);
            assert_eq!(mode(&credentials_path(&home)), 0o600);
        }
        let mut changed = creds;
        changed.pages.clear();
        save(&home, &changed).expect("overwrite");
        assert_eq!(load(&home).expect("load"), Some(changed));
        let leftovers: Vec<_> = std::fs::read_dir(&home)
            .expect("dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec![CREDENTIALS_FILE.to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn existing_loose_directory_is_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        save(dir.path(), &Credentials::default()).expect("save");
        let mode = std::fs::metadata(dir.path())
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn minimal_and_future_files() {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(credentials_path(dir.path()), b"{}").expect("write");
        let c = load(dir.path()).expect("load").expect("some");
        assert_eq!(c, Credentials::default());
        std::fs::write(credentials_path(dir.path()), br#"{"version": 99}"#).expect("write");
        assert!(load(dir.path()).is_err());
        std::fs::write(credentials_path(dir.path()), b"not json").expect("write");
        let err = load(dir.path()).expect_err("bad json").to_string();
        assert!(err.contains("credentials.json"), "{err}");
    }

    #[test]
    fn debug_never_shows_secrets() {
        let shown = format!("{:?}", sample());
        for secret in ["shh-secret", "EAAuser", "EAApage", "EAAwa", "\"vt\""] {
            assert!(!shown.contains(secret), "{shown}");
        }
    }
}
