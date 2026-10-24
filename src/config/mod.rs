//! Configuration: process environment first, then `credentials.json` in `META_MCP_HOME`.

pub mod secret;
pub mod store;

use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use url::Url;

use crate::files::FileJail;
pub use secret::Secret;
pub use store::{
    Credentials, PageConfig, PhoneNumber, StoredToken, StoredWebhook, TokenKind, WhatsAppConfig,
};

pub const DEFAULT_GRAPH_BASE: &str = "https://graph.facebook.com";
pub const DEFAULT_GRAPH_VERSION: &str = "v26.0";
pub const DEFAULT_MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;
pub const DEFAULT_HTTP_HOST: &str = "127.0.0.1";
pub const DEFAULT_HTTP_PORT: u16 = 8788;
pub const DEFAULT_WEBHOOK_LISTEN: &str = "127.0.0.1:8787";
/// Minimum length of a `META_MCP_API_KEYS` entry.
pub const MIN_API_KEY_LEN: usize = 32;

/// A startup configuration problem, printed to stderr.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ConfigError(pub String);

/// Graph API endpoint and app identity shared by every call.
#[derive(Debug, Clone)]
pub struct GraphSettings {
    pub base_url: String,
    pub version: String,
    pub app_id: Option<String>,
    pub app_secret: Option<Secret>,
}

impl Default for GraphSettings {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_GRAPH_BASE.to_string(),
            version: DEFAULT_GRAPH_VERSION.to_string(),
            app_id: None,
            app_secret: None,
        }
    }
}

impl GraphSettings {
    /// `{app_id}|{app_secret}`, the app access token, when both are known.
    pub fn app_token(&self) -> Option<Secret> {
        let id = self.app_id.as_deref().filter(|s| !s.is_empty())?;
        let secret = self.app_secret.as_ref().filter(|s| !s.is_empty())?;
        Some(Secret::new(format!("{id}|{}", secret.expose())))
    }
}

/// Webhook receiver settings.
#[derive(Debug, Clone)]
pub struct WebhookConfig {
    pub verify_token: Option<Secret>,
    pub public_url: Option<Url>,
    pub listen: SocketAddr,
}

/// HTTP transport settings.
#[derive(Clone)]
pub struct HttpConfig {
    pub host: String,
    pub port: u16,
    pub api_keys: Vec<String>,
    pub allowed_hosts: Vec<String>,
}

impl fmt::Debug for HttpConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field(
                "api_keys",
                &format_args!("[{} redacted]", self.api_keys.len()),
            )
            .field("allowed_hosts", &self.allowed_hosts)
            .finish()
    }
}

/// Everything the server needs, resolved once at startup.
#[derive(Debug, Clone)]
pub struct Config {
    pub graph: GraphSettings,
    pub pages: Vec<PageConfig>,
    pub whatsapp: Option<WhatsAppConfig>,
    pub user_token: Option<StoredToken>,
    pub webhook: WebhookConfig,
    pub inbox_db: PathBuf,
    pub files: Option<FileJail>,
    pub max_file_bytes: u64,
    pub read_only: bool,
    pub http: HttpConfig,
    pub download_hosts: Vec<String>,
    pub home: PathBuf,
}

fn list(raw: Option<String>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_bool(name: &str, raw: Option<String>) -> Result<bool, ConfigError> {
    match raw.as_deref().map(|s| s.trim().to_ascii_lowercase()) {
        None => Ok(false),
        Some(s) => match s.as_str() {
            "" | "0" | "false" | "no" | "off" => Ok(false),
            "1" | "true" | "yes" | "on" => Ok(true),
            _ => Err(ConfigError(format!("{name} must be true or false"))),
        },
    }
}

fn non_empty(raw: Option<String>) -> Option<String> {
    raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// True for `localhost` and loopback IPs.
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost")
        || h.parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn check_base_url(base: &str) -> Result<(), ConfigError> {
    let url = Url::parse(base)
        .map_err(|_| ConfigError("META_GRAPH_BASE_URL is not a valid URL".into()))?;
    match url.scheme() {
        "https" => Ok(()),
        "http" if url.host_str().is_some_and(is_loopback_host) => Ok(()),
        _ => Err(ConfigError(
            "META_GRAPH_BASE_URL must use https (plain http is only allowed for localhost or \
             loopback addresses), otherwise tokens would travel unencrypted"
                .into(),
        )),
    }
}

fn check_version(version: &str) -> Result<(), ConfigError> {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let ok = version
        .strip_prefix('v')
        .and_then(|v| v.split_once('.'))
        .is_some_and(|(major, minor)| digits(major) && digits(minor));
    if ok {
        Ok(())
    } else {
        Err(ConfigError(format!(
            "META_GRAPH_VERSION must look like v26.0, got `{version}`"
        )))
    }
}

fn parse_u64(name: &str, raw: Option<String>, default: u64) -> Result<u64, ConfigError> {
    match non_empty(raw) {
        Some(s) => s
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| ConfigError(format!("{name} must be a positive integer"))),
        None => Ok(default),
    }
}

fn default_dirs() -> Result<directories::ProjectDirs, ConfigError> {
    directories::ProjectDirs::from("", "", "meta-mcp").ok_or_else(|| {
        ConfigError(
            "cannot determine the home directory: set META_MCP_HOME and META_MCP_INBOX_DB".into(),
        )
    })
}

/// `META_MCP_HOME` default: the platform config directory for `meta-mcp`.
pub fn default_home() -> Result<PathBuf, ConfigError> {
    Ok(default_dirs()?.config_dir().to_path_buf())
}

fn whatsapp_from(
    env_token: Option<Secret>,
    waba_env: Option<String>,
    phone_env: Option<String>,
    stored: Option<WhatsAppConfig>,
) -> Option<WhatsAppConfig> {
    if let Some(token) = env_token {
        return Some(WhatsAppConfig {
            waba_id: waba_env,
            token,
            token_kind: TokenKind::SystemUser,
            phone_numbers: phone_env.map(bare_phone).into_iter().collect(),
        });
    }
    let mut wa = stored?;
    if waba_env.is_some() {
        wa.waba_id = waba_env;
    }
    if let Some(id) = phone_env {
        let known = wa.phone_numbers.iter().find(|p| p.id == id).cloned();
        wa.phone_numbers = vec![known.unwrap_or_else(|| bare_phone(id))];
    }
    Some(wa)
}

fn bare_phone(id: String) -> PhoneNumber {
    PhoneNumber {
        id,
        display_phone_number: String::new(),
        verified_name: String::new(),
    }
}

impl Config {
    /// Reads the process environment and the credentials file.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|k| std::env::var(k).ok(), None)
    }

    /// Reads configuration through `get`; `home` overrides `META_MCP_HOME` and the default.
    pub fn from_lookup(
        get: impl Fn(&str) -> Option<String>,
        home: Option<PathBuf>,
    ) -> Result<Self, ConfigError> {
        let home = match home.or_else(|| non_empty(get("META_MCP_HOME")).map(PathBuf::from)) {
            Some(h) => h,
            None => default_home()?,
        };
        let creds = store::load(&home)?.unwrap_or_default();

        let base_url = non_empty(get("META_GRAPH_BASE_URL"))
            .map(|s| s.trim_end_matches('/').to_string())
            .unwrap_or_else(|| DEFAULT_GRAPH_BASE.to_string());
        check_base_url(&base_url)?;
        let version =
            non_empty(get("META_GRAPH_VERSION")).unwrap_or_else(|| DEFAULT_GRAPH_VERSION.into());
        check_version(&version)?;
        let graph = GraphSettings {
            base_url,
            version,
            app_id: non_empty(get("META_APP_ID")).or(creds.app_id),
            app_secret: non_empty(get("META_APP_SECRET"))
                .map(Secret::new)
                .or(creds.app_secret)
                .filter(|s| !s.is_empty()),
        };

        let pages = match non_empty(get("META_PAGE_ACCESS_TOKEN")) {
            Some(token) => vec![PageConfig {
                id: non_empty(get("META_PAGE_ID")).unwrap_or_default(),
                name: String::new(),
                token: Secret::new(token),
            }],
            None => creds.pages,
        };
        let whatsapp = whatsapp_from(
            non_empty(get("META_WHATSAPP_ACCESS_TOKEN")).map(Secret::new),
            non_empty(get("META_WABA_ID")),
            non_empty(get("META_WHATSAPP_PHONE_NUMBER_ID")),
            creds.whatsapp,
        );

        let public_url =
            match non_empty(get("META_WEBHOOK_PUBLIC_URL")).or(creds.webhook.public_url) {
                Some(raw) => {
                    let url = Url::parse(&raw).map_err(|_| {
                        ConfigError("META_WEBHOOK_PUBLIC_URL is not a valid URL".into())
                    })?;
                    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                        return Err(ConfigError(
                            "META_WEBHOOK_PUBLIC_URL must be an http(s) URL with a host".into(),
                        ));
                    }
                    Some(url)
                }
                None => None,
            };
        let listen_raw = non_empty(get("META_MCP_WEBHOOK_LISTEN"))
            .unwrap_or_else(|| DEFAULT_WEBHOOK_LISTEN.to_string());
        let listen: SocketAddr = listen_raw.parse().map_err(|_| {
            ConfigError("META_MCP_WEBHOOK_LISTEN must look like 127.0.0.1:8787".into())
        })?;
        let webhook = WebhookConfig {
            verify_token: non_empty(get("META_WEBHOOK_VERIFY_TOKEN"))
                .map(Secret::new)
                .or(creds.webhook.verify_token)
                .filter(|s| !s.is_empty()),
            public_url,
            listen,
        };

        let inbox_db = match non_empty(get("META_MCP_INBOX_DB")) {
            Some(p) => PathBuf::from(p),
            None => default_dirs()?.data_dir().join("inbox.db"),
        };
        let files = match non_empty(get("META_MCP_FILES_DIR")) {
            Some(dir) => Some(
                FileJail::new(Path::new(&dir))
                    .map_err(|e| ConfigError(format!("META_MCP_FILES_DIR: {e}")))?,
            ),
            None => None,
        };
        let port = match non_empty(get("META_MCP_PORT")) {
            Some(s) => s
                .parse::<u16>()
                .map_err(|_| ConfigError("META_MCP_PORT must be a port number".into()))?,
            None => DEFAULT_HTTP_PORT,
        };

        Ok(Self {
            graph,
            pages,
            whatsapp,
            user_token: creds.user_token,
            webhook,
            inbox_db,
            files,
            max_file_bytes: parse_u64(
                "META_MCP_MAX_FILE_BYTES",
                get("META_MCP_MAX_FILE_BYTES"),
                DEFAULT_MAX_FILE_BYTES,
            )?,
            read_only: parse_bool("META_MCP_READ_ONLY", get("META_MCP_READ_ONLY"))?,
            http: HttpConfig {
                host: non_empty(get("META_MCP_HOST")).unwrap_or_else(|| DEFAULT_HTTP_HOST.into()),
                port,
                api_keys: list(get("META_MCP_API_KEYS")),
                allowed_hosts: list(get("META_MCP_ALLOWED_HOSTS")),
            },
            download_hosts: list(get("META_MCP_DOWNLOAD_HOSTS"))
                .into_iter()
                .map(|h| h.to_ascii_lowercase())
                .collect(),
            home,
        })
    }

    /// HTTP mode refuses to start without at least one strong API key.
    pub fn require_api_keys(&self) -> Result<(), ConfigError> {
        let keys = &self.http.api_keys;
        if keys.is_empty() {
            return Err(ConfigError(
                "META_MCP_API_KEYS is required in http mode: set a comma-separated list of \
                 secret API keys (for example the output of `openssl rand -hex 32`)"
                    .into(),
            ));
        }
        if keys.iter().any(|k| k.chars().count() < MIN_API_KEY_LEN) {
            return Err(ConfigError(format!(
                "every META_MCP_API_KEYS entry must be at least {MIN_API_KEY_LEN} characters; \
                 generate one with `openssl rand -hex 32`"
            )));
        }
        Ok(())
    }

    /// True while an env-provided page token still lacks its page id.
    pub fn needs_page_resolution(&self) -> bool {
        self.pages.iter().any(|p| p.id.is_empty())
    }

    /// The page for `page_id`, or the only configured page when it is `None`.
    pub fn page(&self, page_id: Option<&str>) -> Result<&PageConfig, ConfigError> {
        let wanted = page_id.map(str::trim).filter(|s| !s.is_empty());
        let ids = || {
            self.pages
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        match (wanted, self.pages.as_slice()) {
            (_, []) => Err(ConfigError(
                "no page configured: set META_PAGE_ACCESS_TOKEN or run `meta-mcp setup`".into(),
            )),
            (Some(id), pages) => pages.iter().find(|p| p.id == id).ok_or_else(|| {
                ConfigError(format!(
                    "page `{id}` is not configured (configured: {})",
                    ids()
                ))
            }),
            (None, [one]) if one.id.is_empty() => Err(ConfigError(
                "the page id is not resolved yet: set META_PAGE_ID or fix the page token".into(),
            )),
            (None, [one]) => Ok(one),
            (None, _) => Err(ConfigError(format!(
                "several pages configured, pass page_id (one of: {})",
                ids()
            ))),
        }
    }

    /// The WhatsApp block and number for `phone_number_id`, or the only number when `None`.
    pub fn phone(
        &self,
        phone_number_id: Option<&str>,
    ) -> Result<(&WhatsAppConfig, &PhoneNumber), ConfigError> {
        let wa = self.whatsapp.as_ref().ok_or_else(|| {
            ConfigError(
                "WhatsApp is not configured: set META_WHATSAPP_ACCESS_TOKEN and \
                 META_WHATSAPP_PHONE_NUMBER_ID or run `meta-mcp setup`"
                    .into(),
            )
        })?;
        let wanted = phone_number_id.map(str::trim).filter(|s| !s.is_empty());
        let ids = || {
            wa.phone_numbers
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        match (wanted, wa.phone_numbers.as_slice()) {
            (_, []) => Err(ConfigError(
                "no WhatsApp phone number configured: set META_WHATSAPP_PHONE_NUMBER_ID or run \
                 `meta-mcp setup`"
                    .into(),
            )),
            (Some(id), numbers) => numbers
                .iter()
                .find(|p| p.id == id)
                .map(|p| (wa, p))
                .ok_or_else(|| {
                    ConfigError(format!(
                        "phone number `{id}` is not configured (configured: {})",
                        ids()
                    ))
                }),
            (None, [one]) => Ok((wa, one)),
            (None, _) => Err(ConfigError(format!(
                "several phone numbers configured, pass phone_number_id (one of: {})",
                ids()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    fn cfg_in(home: &Path, pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
        Config::from_lookup(lookup(pairs), Some(home.to_path_buf()))
    }

    fn stored() -> Credentials {
        Credentials {
            app_id: Some("stored-app".into()),
            app_secret: Some(Secret::new("stored-secret")),
            pages: vec![
                PageConfig {
                    id: "1".into(),
                    name: "One".into(),
                    token: Secret::new("EAAstoredone"),
                },
                PageConfig {
                    id: "2".into(),
                    name: "Two".into(),
                    token: Secret::new("EAAstoredtwo"),
                },
            ],
            whatsapp: Some(WhatsAppConfig {
                waba_id: Some("stored-waba".into()),
                token: Secret::new("EAAstoredwa"),
                token_kind: TokenKind::User,
                phone_numbers: vec![
                    PhoneNumber {
                        id: "p1".into(),
                        display_phone_number: "+1 555 0101".into(),
                        verified_name: "One".into(),
                    },
                    PhoneNumber {
                        id: "p2".into(),
                        display_phone_number: "+1 555 0102".into(),
                        verified_name: "Two".into(),
                    },
                ],
            }),
            webhook: StoredWebhook {
                verify_token: Some(Secret::new("stored-vt")),
                public_url: Some("https://stored.example.org/webhook".into()),
            },
            ..Credentials::default()
        }
    }

    #[test]
    fn defaults_without_any_source() {
        let dir = tempfile::tempdir().expect("tmp");
        let c = cfg_in(dir.path(), &[]).expect("config");
        assert_eq!(c.graph.base_url, DEFAULT_GRAPH_BASE);
        assert_eq!(c.graph.version, "v26.0");
        assert!(c.graph.app_id.is_none() && c.graph.app_secret.is_none());
        assert!(c.pages.is_empty() && c.whatsapp.is_none() && c.user_token.is_none());
        assert_eq!(c.http.host, "127.0.0.1");
        assert_eq!(c.http.port, 8788);
        assert_eq!(c.webhook.listen.to_string(), "127.0.0.1:8787");
        assert_eq!(c.max_file_bytes, 100 * 1024 * 1024);
        assert!(!c.read_only && c.files.is_none());
        assert_eq!(c.home, dir.path());
        assert!(c.page(None).is_err());
        assert!(c.phone(None).is_err());
    }

    #[test]
    fn env_precedence() {
        let dir = tempfile::tempdir().expect("tmp");
        store::save(dir.path(), &stored()).expect("save");

        let from_file = cfg_in(dir.path(), &[]).expect("config");
        assert_eq!(from_file.graph.app_id.as_deref(), Some("stored-app"));
        assert_eq!(from_file.pages.len(), 2);
        assert_eq!(
            from_file.webhook.verify_token.as_ref().map(Secret::expose),
            Some("stored-vt")
        );
        assert_eq!(
            from_file.webhook.public_url.as_ref().map(Url::as_str),
            Some("https://stored.example.org/webhook")
        );
        assert_eq!(
            from_file.whatsapp.as_ref().map(|w| w.phone_numbers.len()),
            Some(2)
        );

        let from_env = cfg_in(
            dir.path(),
            &[
                ("META_APP_ID", "env-app"),
                ("META_APP_SECRET", "env-secret"),
                ("META_PAGE_ACCESS_TOKEN", "EAAenvpage"),
                ("META_PAGE_ID", "99"),
                ("META_WHATSAPP_ACCESS_TOKEN", "EAAenvwa"),
                ("META_WHATSAPP_PHONE_NUMBER_ID", "envphone"),
                ("META_WABA_ID", "envwaba"),
                ("META_WEBHOOK_VERIFY_TOKEN", "env-vt"),
                ("META_WEBHOOK_PUBLIC_URL", "https://env.example.org/webhook"),
            ],
        )
        .expect("config");
        assert_eq!(from_env.graph.app_id.as_deref(), Some("env-app"));
        assert_eq!(
            from_env.graph.app_secret.as_ref().map(Secret::expose),
            Some("env-secret")
        );
        assert_eq!(from_env.pages.len(), 1, "env page replaces the stored list");
        assert_eq!(from_env.pages[0].id, "99");
        assert_eq!(from_env.pages[0].token.expose(), "EAAenvpage");
        let wa = from_env.whatsapp.as_ref().expect("whatsapp");
        assert_eq!(wa.token.expose(), "EAAenvwa");
        assert_eq!(wa.waba_id.as_deref(), Some("envwaba"));
        assert_eq!(wa.phone_numbers.len(), 1);
        assert_eq!(wa.phone_numbers[0].id, "envphone");
        assert_eq!(
            from_env.webhook.verify_token.as_ref().map(Secret::expose),
            Some("env-vt")
        );
        assert_eq!(
            from_env.webhook.public_url.as_ref().map(Url::as_str),
            Some("https://env.example.org/webhook")
        );
    }

    #[test]
    fn env_phone_id_narrows_the_stored_whatsapp_block() {
        let dir = tempfile::tempdir().expect("tmp");
        store::save(dir.path(), &stored()).expect("save");
        let c = cfg_in(dir.path(), &[("META_WHATSAPP_PHONE_NUMBER_ID", "p2")]).expect("config");
        let wa = c.whatsapp.as_ref().expect("whatsapp");
        assert_eq!(wa.token.expose(), "EAAstoredwa");
        assert_eq!(wa.phone_numbers.len(), 1);
        assert_eq!(wa.phone_numbers[0].display_phone_number, "+1 555 0102");
        let (_, number) = c.phone(None).expect("only number");
        assert_eq!(number.id, "p2");
    }

    #[test]
    fn env_page_without_id_needs_resolution() {
        let dir = tempfile::tempdir().expect("tmp");
        let c = cfg_in(dir.path(), &[("META_PAGE_ACCESS_TOKEN", "EAAtok")]).expect("config");
        assert!(c.needs_page_resolution());
        assert!(c.page(None).is_err());
    }

    #[test]
    fn page_and_phone_selection() {
        let dir = tempfile::tempdir().expect("tmp");
        store::save(dir.path(), &stored()).expect("save");
        let c = cfg_in(dir.path(), &[]).expect("config");
        let err = c.page(None).expect_err("two pages").to_string();
        assert!(
            err.contains("several pages configured, pass page_id"),
            "{err}"
        );
        assert_eq!(c.page(Some("2")).expect("page").name, "Two");
        assert!(c.page(Some("3")).is_err());
        let err = c.phone(None).expect_err("two numbers").to_string();
        assert!(err.contains("several phone numbers configured"), "{err}");
        assert_eq!(c.phone(Some("p1")).expect("phone").1.verified_name, "One");

        let single = cfg_in(
            dir.path(),
            &[("META_PAGE_ACCESS_TOKEN", "EAAx"), ("META_PAGE_ID", "7")],
        )
        .expect("config");
        assert_eq!(single.page(None).expect("only page").id, "7");
        let none = cfg_in(tempfile::tempdir().expect("tmp").path(), &[]).expect("config");
        assert!(
            none.page(None)
                .expect_err("none")
                .to_string()
                .contains("no page configured")
        );
    }

    #[test]
    fn debug_redacts_every_secret() {
        let dir = tempfile::tempdir().expect("tmp");
        store::save(dir.path(), &stored()).expect("save");
        let key = "supersecretkey".repeat(3);
        let c = cfg_in(
            dir.path(),
            &[
                ("META_MCP_API_KEYS", &key),
                ("META_PAGE_ACCESS_TOKEN", "EAAenvpagetoken"),
                ("META_PAGE_ID", "5"),
            ],
        )
        .expect("config");
        let shown = format!("{c:?}");
        for secret in [
            "supersecretkey",
            "EAAenvpagetoken",
            "stored-secret",
            "stored-vt",
            "EAAstoredwa",
        ] {
            assert!(!shown.contains(secret), "{secret} leaked in {shown}");
        }
        assert!(shown.contains("1 redacted"), "{shown}");
    }

    #[test]
    fn http_needs_strong_keys() {
        let dir = tempfile::tempdir().expect("tmp");
        let c = cfg_in(dir.path(), &[]).expect("config");
        assert!(c.require_api_keys().is_err());
        let k1 = "a".repeat(32);
        let k2 = "b".repeat(40);
        let keys = format!(" {k1}, {k2} ");
        let c = cfg_in(dir.path(), &[("META_MCP_API_KEYS", &keys)]).expect("config");
        assert_eq!(c.http.api_keys, vec![k1, k2]);
        assert!(c.require_api_keys().is_ok());
        let c = cfg_in(dir.path(), &[("META_MCP_API_KEYS", "short")]).expect("config");
        let err = c.require_api_keys().expect_err("short").to_string();
        assert!(err.contains("32") && !err.contains("short"), "{err}");
    }

    #[test]
    fn base_url_and_version_are_validated() {
        let dir = tempfile::tempdir().expect("tmp");
        let with = |u: &str| cfg_in(dir.path(), &[("META_GRAPH_BASE_URL", u)]);
        assert!(with("http://example.com").is_err());
        assert!(with("http://10.0.0.5:8080").is_err());
        assert!(with("ftp://localhost").is_err());
        assert!(with("not a url").is_err());
        assert!(with("https://proxy.example.com/").is_ok());
        assert!(with("http://127.0.0.1:9000").is_ok());
        assert!(with("http://localhost:9000").is_ok());
        assert!(with("http://[::1]:9000").is_ok());
        assert_eq!(
            with("https://proxy.example.com/")
                .expect("ok")
                .graph
                .base_url,
            "https://proxy.example.com"
        );
        let version = |v: &str| cfg_in(dir.path(), &[("META_GRAPH_VERSION", v)]);
        assert!(version("v25.0").is_ok());
        assert!(version("26.0").is_err());
        assert!(version("v26").is_err());
        assert!(version("vx.y").is_err());
    }

    #[test]
    fn bools_numbers_and_lists() {
        let dir = tempfile::tempdir().expect("tmp");
        let c = cfg_in(
            dir.path(),
            &[
                ("META_MCP_READ_ONLY", "yes"),
                ("META_MCP_PORT", "9001"),
                ("META_MCP_HOST", "0.0.0.0"),
                ("META_MCP_ALLOWED_HOSTS", "a.example.org, b.example.org:443"),
                ("META_MCP_DOWNLOAD_HOSTS", "CDN.Example.org"),
                ("META_MCP_MAX_FILE_BYTES", "2048"),
                ("META_MCP_WEBHOOK_LISTEN", "0.0.0.0:9999"),
                ("META_MCP_INBOX_DB", "/tmp/x/inbox.db"),
            ],
        )
        .expect("config");
        assert!(c.read_only);
        assert_eq!(c.http.port, 9001);
        assert_eq!(c.http.host, "0.0.0.0");
        assert_eq!(
            c.http.allowed_hosts,
            vec!["a.example.org", "b.example.org:443"]
        );
        assert_eq!(c.download_hosts, vec!["cdn.example.org"]);
        assert_eq!(c.max_file_bytes, 2048);
        assert_eq!(c.webhook.listen.port(), 9999);
        assert_eq!(c.inbox_db, PathBuf::from("/tmp/x/inbox.db"));
        for (name, bad) in [
            ("META_MCP_READ_ONLY", "maybe"),
            ("META_MCP_PORT", "http"),
            ("META_MCP_MAX_FILE_BYTES", "0"),
            ("META_MCP_WEBHOOK_LISTEN", "nope"),
            ("META_WEBHOOK_PUBLIC_URL", "ftp://x"),
        ] {
            assert!(cfg_in(dir.path(), &[(name, bad)]).is_err(), "{name}={bad}");
        }
    }

    #[test]
    fn files_dir_must_exist() {
        let dir = tempfile::tempdir().expect("tmp");
        let missing = dir.path().join("missing");
        let missing = missing.to_str().expect("utf8");
        assert!(cfg_in(dir.path(), &[("META_MCP_FILES_DIR", missing)]).is_err());
        let ok = dir.path().to_str().expect("utf8");
        let c = cfg_in(dir.path(), &[("META_MCP_FILES_DIR", ok)]).expect("config");
        assert!(c.files.is_some());
    }

    #[test]
    fn app_token_needs_both_halves() {
        let mut g = GraphSettings::default();
        assert!(g.app_token().is_none());
        g.app_id = Some("123".into());
        assert!(g.app_token().is_none());
        g.app_secret = Some(Secret::new("abc"));
        assert_eq!(g.app_token().expect("token").expose(), "123|abc");
    }

    #[test]
    fn home_comes_from_meta_mcp_home_env() {
        let dir = tempfile::tempdir().expect("tmp");
        let home = dir.path().to_str().expect("utf8");
        store::save(dir.path(), &stored()).expect("save");
        let c = Config::from_lookup(lookup(&[("META_MCP_HOME", home)]), None).expect("config");
        assert_eq!(c.home, dir.path());
        assert_eq!(c.pages.len(), 2);
    }
}
