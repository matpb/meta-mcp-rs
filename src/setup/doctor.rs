//! Live checks behind `meta-mcp doctor` and the final step of `meta-mcp setup`.

use std::future::Future;
use std::process::ExitCode;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use url::Url;

use super::discover::{
    PAGE_TOKEN_SCOPES, PERMISSION_NOTE, granted_scopes, is_permission_code, missing_scopes,
    phone_status, probe_page,
};
use super::oauth::scopes_for;
use super::report::{self, Status, scrub};
use super::subscribe;
use crate::cli::DoctorArgs;
use crate::config::secret::{Secret, random_hex};
use crate::config::store::credentials_path;
use crate::config::{Config, PageConfig, TokenKind, WhatsAppConfig};
use crate::graph::{GraphClient, GraphError, token};
use crate::inbox::{Inbox, now_secs};

const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
const EXPIRY_WARN_DAYS: i64 = 10;
const QUIET_DAYS: i64 = 7;
const ACCESS_HINT: &str = "gate test: send a message to the Page from an account that has no role in the app; if it never arrives, the app needs Advanced Access for pages_messaging (App Review and Business Verification)";

/// One line of the report.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub detail: String,
    pub fix: Option<String>,
}

impl Check {
    fn make(status: Status, name: impl Into<String>, detail: impl AsRef<str>) -> Self {
        Self {
            name: name.into(),
            status,
            detail: scrub(detail.as_ref()),
            fix: None,
        }
    }

    pub fn ok(name: impl Into<String>, detail: impl AsRef<str>) -> Self {
        Self::make(Status::Ok, name, detail)
    }

    pub fn warn(name: impl Into<String>, detail: impl AsRef<str>) -> Self {
        Self::make(Status::Warn, name, detail)
    }

    pub fn fail(name: impl Into<String>, detail: impl AsRef<str>) -> Self {
        Self::make(Status::Fail, name, detail)
    }

    pub fn info(name: impl Into<String>, detail: impl AsRef<str>) -> Self {
        Self::make(Status::Info, name, detail)
    }

    pub fn with_fix(mut self, fix: impl AsRef<str>) -> Self {
        self.fix = Some(scrub(fix.as_ref()));
        self
    }
}

/// Counts shown on the last line of the report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub ok: usize,
    pub warnings: usize,
    pub failures: usize,
}

impl Summary {
    pub fn line(&self) -> String {
        format!(
            "{} ok, {} warnings, {} failures",
            self.ok, self.warnings, self.failures
        )
    }
}

pub fn summarize(checks: &[Check]) -> Summary {
    let count = |s: Status| checks.iter().filter(|c| c.status == s).count();
    Summary {
        ok: count(Status::Ok),
        warnings: count(Status::Warn),
        failures: count(Status::Fail),
    }
}

/// `✅ name: detail`, followed by a `Fix:` line when the check has one.
pub fn format_check(c: &Check) -> String {
    let mut out = report::status_line(c.status, &format!("{}: {}", c.name, c.detail));
    if let Some(fix) = &c.fix {
        out.push('\n');
        out.push_str(&report::fix_line(fix));
    }
    out
}

#[derive(Serialize)]
struct JsonReport<'a> {
    checks: &'a [Check],
    ok: bool,
    summary: Summary,
}

/// The `--json` document: every check, the verdict and the counts, with fields in a stable order.
pub fn report_json(checks: &[Check]) -> String {
    let summary = summarize(checks);
    let doc = JsonReport {
        checks,
        ok: summary.failures == 0,
        summary,
    };
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

/// What the caller asked the doctor to do beyond the default checks.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    pub webhook: bool,
}

async fn timed<T>(fut: impl Future<Output = Result<T, GraphError>>) -> Result<T, GraphError> {
    tokio::time::timeout(CHECK_TIMEOUT, fut)
        .await
        .unwrap_or_else(|_| Err(GraphError::transport("timed out after 15 seconds")))
}

fn is_dead(e: &GraphError) -> bool {
    e.code() == Some(190)
}

/// The earlier of the stored and the live expiry, so a stale copy on either side warns.
fn combine_expiry(stored: Option<i64>, live: Option<i64>) -> Option<i64> {
    match (stored, live) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Expiry verdict; `verified` says whether Meta confirmed the token, which changes the wording.
fn expiry_check(name: &str, expiry: Option<i64>, now: i64, refresh: &str, verified: bool) -> Check {
    let wording = report::expiry_text(expiry, now);
    match expiry {
        Some(t) if t <= now => Check::fail(name, wording).with_fix("meta-mcp setup"),
        Some(t) if report::days_left(t, now) < EXPIRY_WARN_DAYS => {
            Check::warn(name, wording).with_fix(refresh)
        }
        _ if verified => Check::ok(name, format!("valid, {wording}")),
        _ => Check::ok(name, format!("{wording} (not checked with Meta)")),
    }
}

async fn app_check(graph: &GraphClient, cfg: &Config) -> Check {
    let (Some(app_id), Some(app_token)) = (cfg.graph.app_id.as_deref(), cfg.graph.app_token())
    else {
        return Check::warn(
            "app",
            "no App ID and App Secret: tokens cannot be inspected and webhook signatures cannot be checked",
        )
        .with_fix("set META_APP_ID and META_APP_SECRET, or run `meta-mcp setup`");
    };
    let query = [("fields", "id,name".to_string())];
    match timed(graph.get(app_id, Some(&app_token), &query)).await {
        Ok(v) => {
            let name = v
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unnamed app");
            Check::ok(
                "app",
                format!("{name} ({app_id}) accepted the App ID and App Secret"),
            )
        }
        Err(e) if matches!(e.code(), Some(190 | 101)) => Check::fail(
            "app",
            "the App ID or App Secret was rejected",
        )
        .with_fix(
            "copy them again from Settings > Basic in the App Dashboard, then run `meta-mcp setup --app-secret-env NAME` (or set META_APP_SECRET)",
        ),
        Err(e) => Check::fail("app", format!("cannot verify the app: {e}"))
            .with_fix("check the network and META_GRAPH_BASE_URL, then rerun `meta-mcp doctor`"),
    }
}

fn credentials_check(cfg: &Config) -> Check {
    let path = credentials_path(&cfg.home);
    if !path.exists() {
        return Check::warn(
            "credentials",
            format!(
                "no {} (only environment variables are in use)",
                path.display()
            ),
        )
        .with_fix("meta-mcp setup");
    }
    match report::file_mode(&path) {
        Some(mode) if mode & 0o077 != 0 => Check::warn(
            "credentials",
            format!(
                "{} is readable by other users (mode {mode:o})",
                path.display()
            ),
        )
        .with_fix(format!("chmod 600 {}", path.display())),
        Some(mode) => Check::ok("credentials", format!("{} (mode {mode:o})", path.display())),
        None => Check::ok("credentials", format!("{} exists", path.display())),
    }
}

async fn user_token_checks(
    graph: &GraphClient,
    app_token: Option<&Secret>,
    cfg: &Config,
    now: i64,
) -> Vec<Check> {
    let Some(stored) = &cfg.user_token else {
        return vec![Check::info(
            "user token",
            "none stored: Page tokens are used as they are, and `token refresh` needs a user token",
        )];
    };
    let mut checks = Vec::new();
    let mut live_expiry = None;
    let mut verified = false;
    let mut granted = stored.scopes.clone();
    if let Some(app_token) = app_token {
        match timed(token::debug_token(graph, app_token, &stored.token)).await {
            Ok(d) if !d.is_valid => {
                return vec![
                    Check::fail("user token", "Meta reports the token as no longer valid")
                        .with_fix("meta-mcp setup"),
                ];
            }
            Ok(d) => {
                live_expiry = d.expires_at;
                verified = true;
                let live = granted_scopes(&d);
                if !live.is_empty() {
                    granted = live;
                }
            }
            Err(e) if is_dead(&e) => {
                return vec![
                    Check::fail("user token", format!("rejected: {e}")).with_fix("meta-mcp setup"),
                ];
            }
            Err(e) => checks.push(Check::warn(
                "user token",
                format!("could not inspect the token: {e}"),
            )),
        }
    }
    let expiry = combine_expiry(stored.expires_at, live_expiry);
    if expiry.is_none() && !verified {
        checks.push(Check::info(
            "user token",
            "expiry not recorded and Meta was not asked (the App Secret is unknown or debug_token failed)",
        ));
    } else {
        checks.push(expiry_check(
            "user token",
            expiry,
            now,
            "meta-mcp token refresh",
            verified,
        ));
    }
    if !granted.is_empty() {
        let user_serves_whatsapp = cfg
            .whatsapp
            .as_ref()
            .is_some_and(|w| w.token_kind == TokenKind::User);
        let wanted = scopes_for(!cfg.pages.is_empty(), user_serves_whatsapp, false);
        let missing = missing_scopes(wanted.iter().map(String::as_str), &granted);
        checks.push(if missing.is_empty() {
            Check::ok(
                "user scopes",
                format!("all {} wanted scopes granted", wanted.len()),
            )
        } else {
            let mut fix = "meta-mcp setup --login --rerequest".to_string();
            if user_serves_whatsapp {
                fix.push_str(" --whatsapp");
            }
            if cfg.pages.is_empty() {
                fix.push_str(" --no-messenger");
            }
            Check::warn("user scopes", format!("missing {}", missing.join(", "))).with_fix(fix)
        });
    }
    checks
}

async fn page_checks(
    graph: &GraphClient,
    app_token: Option<&Secret>,
    page: &PageConfig,
    cfg: &Config,
    now: i64,
) -> Vec<Check> {
    let label = format!("page {}", page.id);
    let mut checks = Vec::new();
    if let Some(app_token) = app_token {
        match timed(token::debug_token(graph, app_token, &page.token)).await {
            Ok(d) if !d.is_valid => {
                return vec![
                    Check::fail(&label, "the Page token is no longer valid")
                        .with_fix("meta-mcp setup"),
                ];
            }
            Ok(d) => {
                let kind = d.token_type.as_deref().unwrap_or("PAGE").to_lowercase();
                let refresh = if cfg.user_token.is_some() {
                    "meta-mcp token refresh".to_string()
                } else {
                    format!("meta-mcp setup --login --page-id {}", page.id)
                };
                let mut expiry =
                    expiry_check(&format!("{label} token"), d.expires_at, now, &refresh, true);
                if expiry.status == Status::Ok {
                    expiry.detail = format!(
                        "valid {kind} token, {}",
                        report::expiry_text(d.expires_at, now)
                    );
                }
                checks.push(expiry);
                let granted = granted_scopes(&d);
                let missing = missing_scopes(PAGE_TOKEN_SCOPES, &granted);
                if !granted.is_empty() && !missing.is_empty() {
                    checks.push(
                        Check::warn(
                            format!("{label} scopes"),
                            format!("missing {}", missing.join(", ")),
                        )
                        .with_fix("meta-mcp setup --login --rerequest"),
                    );
                }
            }
            Err(e) if is_dead(&e) => {
                return vec![
                    Check::fail(&label, format!("rejected: {e}")).with_fix("meta-mcp setup"),
                ];
            }
            Err(e) => checks.push(Check::warn(
                format!("{label} token"),
                format!("could not inspect the token: {e}"),
            )),
        }
    }
    let probe =
        match tokio::time::timeout(CHECK_TIMEOUT, probe_page(graph, &page.id, &page.token)).await {
            Ok(p) => p,
            Err(_) => {
                checks.push(
                    Check::fail(&label, "timed out after 15 seconds")
                        .with_fix("check the network, then rerun `meta-mcp doctor`"),
                );
                return checks;
            }
        };
    match probe.name {
        Ok(name) => checks.push(Check::ok(
            &label,
            format!("{name} is readable with this token"),
        )),
        Err(e) if is_dead(&e) => {
            checks.push(Check::fail(&label, format!("rejected: {e}")).with_fix("meta-mcp setup"));
            return checks;
        }
        Err(e) => checks.push(
            Check::fail(&label, format!("cannot read the Page: {e}"))
                .with_fix("check the Page id and token, or run `meta-mcp setup`"),
        ),
    }
    let inbox_label = format!("{label} inbox");
    match probe.conversations {
        Ok(()) => checks.push(Check::ok(&inbox_label, "conversations are readable")),
        Err(e) if is_permission_code(e.code()) => checks.push(
            Check::warn(&inbox_label, format!("conversations refused: {e}"))
                .with_fix(format!("{PERMISSION_NOTE}. {ACCESS_HINT}")),
        ),
        Err(e) if is_dead(&e) => checks
            .push(Check::fail(&inbox_label, format!("rejected: {e}")).with_fix("meta-mcp setup")),
        Err(e) => checks.push(Check::warn(
            &inbox_label,
            format!("the conversations probe failed: {e}"),
        )),
    }
    checks.push(page_subscription_check(graph, page, cfg).await);
    checks
}

async fn page_subscription_check(graph: &GraphClient, page: &PageConfig, cfg: &Config) -> Check {
    let label = format!("page {} webhook", page.id);
    let configured = cfg.webhook.public_url.is_some() && cfg.webhook.verify_token.is_some();
    let app_id = cfg.graph.app_id.as_deref().unwrap_or_default();
    let fields = match timed(subscribe::page_subscription(
        graph,
        &page.id,
        &page.token,
        app_id,
    ))
    .await
    {
        Ok(f) => f,
        Err(e) if configured => {
            return Check::warn(&label, format!("cannot read the Page subscription: {e}"));
        }
        Err(e) => {
            return Check::info(&label, format!("subscription not readable: {e}"));
        }
    };
    let has_messages = fields.iter().any(|f| f == "messages");
    match (configured, has_messages) {
        (true, true) => Check::ok(
            &label,
            format!("subscribed to {} fields, messages included", fields.len()),
        ),
        (true, false) => {
            let url = cfg
                .webhook
                .public_url
                .as_ref()
                .map_or("https://...", Url::as_str);
            Check::warn(
                &label,
                "the app is not subscribed to the messages field on this Page",
            )
            .with_fix(format!("meta-mcp setup --stored --webhook-url {url}"))
        }
        (false, true) => Check::info(&label, format!("subscribed to {} fields", fields.len())),
        (false, false) => Check::info(&label, "not subscribed (the webhook is optional)"),
    }
}

async fn app_subscription_check(
    graph: &GraphClient,
    app_token: &Secret,
    app_id: &str,
    public: &Url,
) -> Check {
    match timed(subscribe::app_subscriptions(graph, app_token, app_id)).await {
        Err(e) => Check::warn(
            "app subscription",
            format!("cannot read the app subscriptions: {e}"),
        ),
        Ok(subs) => {
            let fix = format!("meta-mcp setup --stored --webhook-url {public}");
            match subs.iter().find(|s| s.object == "page") {
                None => Check::warn("app subscription", "no object=page subscription on the app")
                    .with_fix(fix),
                Some(s) if !subscribe::same_url(&s.callback_url, public) => Check::warn(
                    "app subscription",
                    format!("object=page calls {} instead of {public}", s.callback_url),
                )
                .with_fix(fix),
                Some(s) if !s.fields.iter().any(|f| f == "messages") => Check::warn(
                    "app subscription",
                    "object=page does not include the messages field",
                )
                .with_fix(fix),
                Some(s) if !s.active => Check::warn(
                    "app subscription",
                    "object=page is inactive: Meta delivers nothing until it is reactivated",
                )
                .with_fix(fix),
                Some(s) => Check::ok(
                    "app subscription",
                    format!("object=page calls {public} ({} fields)", s.fields.len()),
                ),
            }
        }
    }
}

async fn whatsapp_checks(
    graph: &GraphClient,
    app_token: Option<&Secret>,
    wa: &WhatsAppConfig,
    cfg: &Config,
    now: i64,
) -> Vec<Check> {
    let mut checks = Vec::new();
    if let Some(app_token) = app_token {
        match timed(token::debug_token(graph, app_token, &wa.token)).await {
            Ok(d) if !d.is_valid => {
                checks.push(
                    Check::fail("whatsapp token", "Meta reports the token as no longer valid")
                        .with_fix(match wa.token_kind {
                            TokenKind::SystemUser => "create a new System User token and run `meta-mcp token import --whatsapp-token <token>`",
                            TokenKind::User => "sign in again with `meta-mcp setup --login --whatsapp --rerequest`",
                        }),
                );
                return checks;
            }
            Ok(d) => {
                checks.push(expiry_check(
                    "whatsapp token",
                    d.expires_at,
                    now,
                    "meta-mcp token refresh --whatsapp",
                    true,
                ));
                let wanted = [
                    "whatsapp_business_messaging",
                    "whatsapp_business_management",
                ];
                let granted = granted_scopes(&d);
                let missing = missing_scopes(wanted, &granted);
                if !granted.is_empty() && !missing.is_empty() {
                    checks.push(
                        Check::warn("whatsapp scopes", format!("missing {}", missing.join(", ")))
                            .with_fix("generate the System User token with both whatsapp_business_* permissions"),
                    );
                }
            }
            Err(e) => checks.push(Check::warn(
                "whatsapp token",
                format!("could not inspect the token: {e}"),
            )),
        }
    }
    if wa.phone_numbers.is_empty() {
        checks.push(
            Check::warn("whatsapp phone", "no phone number is stored").with_fix(format!(
                "meta-mcp setup --stored --whatsapp --phone-number-id <id>{}",
                if cfg.pages.is_empty() {
                    " --no-messenger"
                } else {
                    ""
                }
            )),
        );
    }
    for phone in &wa.phone_numbers {
        let label = format!("whatsapp phone {}", phone.id);
        match timed(phone_status(graph, &wa.token, &phone.id)).await {
            Ok(s) => checks.push(Check::ok(&label, s.summary())),
            Err(e) => checks.push(
                Check::fail(&label, format!("cannot read the number: {e}")).with_fix(match wa.token_kind {
                    TokenKind::SystemUser => "give the System User access to this WhatsApp account (Business settings > System users > Assign assets), or run `meta-mcp token import --whatsapp-token <token>` with a token that has it",
                    TokenKind::User => "the user token needs whatsapp_business_management and access to this account: run `meta-mcp setup --login --whatsapp --rerequest`",
                }),
            ),
        }
    }
    let app_id = cfg.graph.app_id.as_deref().unwrap_or_default();
    if let (Some(waba), false) = (wa.waba_id.as_deref(), app_id.is_empty()) {
        let label = format!("whatsapp account {waba}");
        let configured = cfg.webhook.public_url.is_some();
        let read = timed(subscribe::waba_subscription(graph, waba, &wa.token, app_id)).await;
        checks.push(match read {
            Ok(sub) if sub.subscribed => match (&sub.override_uri, &cfg.webhook.public_url) {
                (Some(u), Some(public)) if !subscribe::same_url(u, public) => Check::warn(
                    &label,
                    format!("subscribed, but its callback override {u} sends events there instead of {public}"),
                )
                .with_fix(format!(
                    "meta-mcp setup --stored --webhook-url {public} --webhook-override"
                )),
                (Some(u), _) => Check::ok(
                    &label,
                    format!("this app is subscribed to the account (callback override {u})"),
                ),
                (None, _) => Check::ok(&label, "this app is subscribed to the account"),
            },
            Ok(_) if configured => Check::warn(
                &label,
                "this app is not subscribed to the account: inbound messages will not arrive",
            )
            .with_fix(format!(
                "meta-mcp setup --stored --webhook-url {}",
                cfg.webhook
                    .public_url
                    .as_ref()
                    .map_or("<your public URL>".to_string(), Url::to_string)
            )),
            Ok(_) => Check::info(
                &label,
                "this app is not subscribed (the webhook is optional; sending works without it)",
            ),
            Err(e) => Check::warn(
                &label,
                format!("cannot read the account subscriptions: {e}"),
            ),
        });
    }
    checks
}

async fn webhook_checks(cfg: &Config, opts: &Options) -> Vec<Check> {
    let mut checks = Vec::new();
    match (&cfg.webhook.public_url, &cfg.webhook.verify_token) {
        (None, _) => {
            checks.push(Check::info(
                "webhook",
                "not configured: only needed for inbound messages and the local inbox (`meta-mcp setup --stored --webhook-url https://...`)",
            ));
            if opts.webhook {
                checks.push(
                    Check::warn("webhook reachability", "there is no public URL to test").with_fix(
                        "meta-mcp setup --stored --webhook-url https://your-host/webhook",
                    ),
                );
            }
        }
        (Some(url), None) => checks.push(
            Check::warn(
                "webhook",
                format!("public URL {url} is set but there is no verify token"),
            )
            .with_fix(format!("meta-mcp setup --stored --webhook-url {url}")),
        ),
        (Some(url), Some(verify)) => {
            checks.push(Check::ok(
                "webhook",
                format!("public URL {url}, verify token set"),
            ));
            if opts.webhook {
                checks.push(match subscribe::check_callback(url, verify).await {
                    Ok(()) => Check::ok("webhook reachability", "the callback answered the verification handshake"),
                    Err(why) => Check::fail("webhook reachability", format!("{url}: {why}")).with_fix(
                        "start `meta-mcp webhook` behind your HTTPS tunnel, then rerun `meta-mcp doctor --webhook`",
                    ),
                });
            }
        }
    }
    checks
}

fn age_text(seconds: i64) -> String {
    match seconds {
        s if s < 90 => "just now".to_string(),
        s if s < 5_400 => format!("{} minutes ago", s / 60),
        s if s < 172_800 => format!("{} hours ago", s / 3600),
        s => format!("{} days ago", s / 86_400),
    }
}

fn inbox_check(cfg: &Config, now: i64) -> Check {
    let webhook_on = cfg.webhook.public_url.is_some();
    if !cfg.inbox_db.exists() {
        return if webhook_on {
            Check::warn("inbox", "no inbox database yet")
                .with_fix("start `meta-mcp webhook` so events are stored")
        } else {
            Check::info(
                "inbox",
                "no database yet (created when the webhook receiver first runs)",
            )
        };
    }
    let stats = Inbox::open(&cfg.inbox_db).and_then(|i| i.stats());
    match stats {
        Err(e) => Check::fail(
            "inbox",
            format!("cannot open {}: {e}", cfg.inbox_db.display()),
        )
        .with_fix("check the permissions of the file or set META_MCP_INBOX_DB"),
        Ok(s) => {
            let last = s.last_received_at.map(|t| now - t);
            let when = last.map_or("no event yet".to_string(), |a| {
                format!("last event {}", age_text(a))
            });
            let detail = format!("{} events, {} unprocessed, {when}", s.total, s.unprocessed);
            if webhook_on && last.is_none_or(|a| a > QUIET_DAYS * 86_400) {
                Check::warn("inbox", format!("{detail}; nothing in {QUIET_DAYS} days"))
                    .with_fix("send a test event from the App Dashboard (Webhooks > Test) and check the tunnel")
            } else {
                Check::ok("inbox", detail)
            }
        }
    }
}

fn files_check(cfg: &Config) -> Check {
    let Some(jail) = &cfg.files else {
        return Check::info(
            "files",
            "file tools are off (set META_MCP_FILES_DIR to a directory to enable them)",
        );
    };
    let probe = format!("meta-mcp-doctor-{}.tmp", random_hex(4));
    match jail.write_new(&probe, b"ok", false, 16) {
        Ok(path) => {
            let _ = std::fs::remove_file(path);
            Check::info("files", format!("{} is writable", jail.root().display()))
        }
        Err(e) => Check::warn(
            "files",
            format!("{} is not writable: {e}", jail.root().display()),
        )
        .with_fix("fix the directory permissions or point META_MCP_FILES_DIR elsewhere"),
    }
}

/// Registers every stored secret so nothing printed can carry one.
fn guard_secrets(cfg: &Config) {
    let tokens = cfg
        .pages
        .iter()
        .map(|p| &p.token)
        .chain(cfg.user_token.iter().map(|t| &t.token))
        .chain(cfg.whatsapp.iter().map(|w| &w.token))
        .chain(cfg.graph.app_secret.iter())
        .chain(cfg.webhook.verify_token.iter());
    for secret in tokens {
        report::guard(secret);
    }
    if let Some(app_token) = cfg.graph.app_token() {
        report::guard(&app_token);
    }
}

/// Runs every check in order, handing each finished one to `emit` so text output can stream.
pub async fn run_checks(cfg: &Config, opts: &Options, mut emit: impl FnMut(&Check)) -> Vec<Check> {
    let mut all: Vec<Check> = Vec::new();
    let mut push = |checks: Vec<Check>, all: &mut Vec<Check>| {
        for c in checks {
            emit(&c);
            all.push(c);
        }
    };
    let now = now_secs();
    let mut cfg = cfg.clone();
    guard_secrets(&cfg);
    if cfg.pages.is_empty() && cfg.whatsapp.is_none() {
        push(
            vec![
                Check::fail("setup", "no Page and no WhatsApp number is configured")
                    .with_fix("meta-mcp setup"),
            ],
            &mut all,
        );
    }
    let graph = match GraphClient::new(&cfg.graph, Vec::new()) {
        Ok(g) => g,
        Err(e) => {
            push(
                vec![Check::fail("graph", e.to_string()).with_fix("check META_GRAPH_BASE_URL")],
                &mut all,
            );
            return all;
        }
    };
    if cfg.needs_page_resolution() {
        let resolved =
            tokio::time::timeout(CHECK_TIMEOUT, crate::server::resolve_env_page(&mut cfg)).await;
        let failure = match resolved {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(format!("{e:#}")),
            Err(_) => Some("timed out after 15 seconds".to_string()),
        };
        if let Some(message) = failure {
            push(
                vec![
                    Check::fail("pages", message)
                        .with_fix("set META_PAGE_ID or fix META_PAGE_ACCESS_TOKEN"),
                ],
                &mut all,
            );
            cfg.pages.retain(|p| !p.id.is_empty());
        }
    }
    let app_token = cfg.graph.app_token();
    push(vec![app_check(&graph, &cfg).await], &mut all);
    push(vec![credentials_check(&cfg)], &mut all);
    push(
        user_token_checks(&graph, app_token.as_ref(), &cfg, now).await,
        &mut all,
    );
    for page in &cfg.pages {
        push(
            page_checks(&graph, app_token.as_ref(), page, &cfg, now).await,
            &mut all,
        );
    }
    if let (Some(url), Some(token), Some(app_id), false) = (
        cfg.webhook.public_url.as_ref(),
        app_token.as_ref(),
        cfg.graph.app_id.as_deref(),
        cfg.pages.is_empty(),
    ) {
        push(
            vec![app_subscription_check(&graph, token, app_id, url).await],
            &mut all,
        );
    }
    if let Some(wa) = &cfg.whatsapp {
        push(
            whatsapp_checks(&graph, app_token.as_ref(), wa, &cfg, now).await,
            &mut all,
        );
    }
    push(webhook_checks(&cfg, opts).await, &mut all);
    push(vec![inbox_check(&cfg, now)], &mut all);
    push(vec![files_check(&cfg)], &mut all);
    push(
        vec![Check::info(
            "mode",
            if cfg.read_only {
                "read-only mode is on: send and write tools are hidden"
            } else {
                "read-only mode is off"
            },
        )],
        &mut all,
    );
    if !cfg.pages.is_empty() {
        push(vec![Check::info("access", ACCESS_HINT)], &mut all);
    }
    all
}

/// Prints the text report (streamed) and the summary; true when there is no failure.
pub async fn print_report(cfg: &Config, opts: &Options) -> bool {
    let checks = run_checks(cfg, opts, |c| report::say(&format_check(c))).await;
    report::say("");
    let summary = summarize(&checks);
    report::say(&summary.line());
    summary.failures == 0
}

/// The `doctor` command.
pub async fn run(args: &DoctorArgs) -> ExitCode {
    let opts = Options {
        webhook: args.webhook,
    };
    let checks = match Config::from_env() {
        Ok(cfg) if args.json => run_checks(&cfg, &opts, |_| {}).await,
        Ok(cfg) => {
            report::say("meta-mcp doctor");
            return exit_code(print_report(&cfg, &opts).await);
        }
        Err(e) => vec![Check::fail("config", e.to_string()).with_fix(super::config_fix(&e))],
    };
    if args.json {
        report::say(&report_json(&checks));
    } else {
        for c in &checks {
            report::say(&format_check(c));
        }
        report::say("");
        report::say(&summarize(&checks).line());
    }
    exit_code(summarize(&checks).failures == 0)
}

fn exit_code(ok: bool) -> ExitCode {
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn lines_start_with_the_emoji_and_carry_the_fix() {
        let ok = Check::ok("app", "Test App (1) accepted the App ID and App Secret");
        assert_eq!(
            format_check(&ok),
            "✅ app: Test App (1) accepted the App ID and App Secret"
        );
        let bad =
            Check::fail("page 42", "the Page token is no longer valid").with_fix("meta-mcp setup");
        assert_eq!(
            format_check(&bad),
            "❌ page 42: the Page token is no longer valid\n   Fix: meta-mcp setup"
        );
        assert!(format_check(&Check::warn("x", "y")).starts_with("⚠️ x: y"));
        assert!(format_check(&Check::info("x", "y")).starts_with("ℹ️ x: y"));
    }

    #[test]
    fn details_and_fixes_are_scrubbed() {
        let tok = format!("EAA{}", "x".repeat(30));
        let c = Check::fail("t", format!("bad {tok}")).with_fix(format!("use {tok}"));
        assert_eq!(c.detail, "bad EAA***");
        assert_eq!(c.fix.as_deref(), Some("use EAA***"));
    }

    #[test]
    fn summary_counts_and_json_verdict() {
        let checks = vec![
            Check::ok("a", "1"),
            Check::ok("b", "2"),
            Check::warn("c", "3"),
            Check::info("d", "4"),
        ];
        let s = summarize(&checks);
        assert_eq!((s.ok, s.warnings, s.failures), (2, 1, 0));
        assert_eq!(s.line(), "2 ok, 1 warnings, 0 failures");
        let json_of =
            |c: &[Check]| -> Value { serde_json::from_str(&report_json(c)).expect("valid JSON") };
        let v = json_of(&checks);
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["checks"][2]["status"], json!("warn"));
        assert_eq!(v["checks"][0]["fix"], Value::Null);
        assert_eq!(v["summary"]["warnings"], json!(1));
        let text = report_json(&checks);
        assert!(
            text.find("\"name\"") < text.find("\"status\""),
            "fields keep their declared order"
        );
        let with_failure = [checks, vec![Check::fail("e", "5")]].concat();
        assert_eq!(json_of(&with_failure)["ok"], json!(false));
    }

    #[test]
    fn expiry_rules() {
        let day = 86_400;
        assert_eq!(combine_expiry(Some(5), Some(3)), Some(3));
        assert_eq!(combine_expiry(None, Some(3)), Some(3));
        assert_eq!(combine_expiry(Some(5), None), Some(5));
        assert_eq!(combine_expiry(None, None), None);

        let refresh = "meta-mcp token refresh";
        let soon = expiry_check("user token", Some(NOW + 3 * day + 60), NOW, refresh, true);
        assert_eq!(soon.status, Status::Warn);
        assert_eq!(soon.fix.as_deref(), Some("meta-mcp token refresh"));
        let fine = expiry_check("user token", Some(NOW + 40 * day), NOW, refresh, true);
        assert_eq!(fine.status, Status::Ok);
        assert!(
            fine.detail.starts_with("valid, expires in 40 days"),
            "{}",
            fine.detail
        );
        let never = expiry_check("page token", None, NOW, refresh, true);
        assert_eq!(never.status, Status::Ok);
        assert!(never.detail.contains("never expires"));
        let unchecked = expiry_check("user token", None, NOW, refresh, false);
        assert!(
            unchecked.detail.contains("not checked with Meta"),
            "{}",
            unchecked.detail
        );
        let gone = expiry_check("user token", Some(NOW - 5), NOW, refresh, true);
        assert_eq!(gone.status, Status::Fail);
        assert_eq!(gone.fix.as_deref(), Some("meta-mcp setup"));
    }

    #[test]
    fn urls_compare_without_trailing_slash_noise() {
        let public = Url::parse("https://hooks.example.org/webhook").expect("url");
        assert!(subscribe::same_url(
            "https://hooks.example.org/webhook",
            &public
        ));
        assert!(subscribe::same_url(
            "https://hooks.example.org/webhook/",
            &public
        ));
        assert!(!subscribe::same_url(
            "https://other.example.org/webhook",
            &public
        ));
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(age_text(10), "just now");
        assert_eq!(age_text(600), "10 minutes ago");
        assert_eq!(age_text(7_200), "2 hours ago");
        assert_eq!(age_text(3 * 86_400), "3 days ago");
    }
}
