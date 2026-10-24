//! Guided setup (`meta-mcp setup`), the doctor and the token commands.

pub mod discover;
pub mod doctor;
pub mod oauth;
pub mod prompt;
pub mod report;
pub mod subscribe;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;
use url::Url;

use crate::cli::{DoctorArgs, ImportArgs, SetupArgs, TokenArgs, TokenCommand};
use crate::config::secret::{Secret, random_hex};
use crate::config::store::{self, CREDENTIALS_VERSION, Credentials, credentials_path};
use crate::config::{
    Config, ConfigError, GraphSettings, PageConfig, PhoneNumber, StoredToken, TokenKind,
    WhatsAppConfig,
};
use crate::graph::token::{self, DebugToken, PageInfo};
use crate::graph::{GraphClient, GraphError};
use crate::inbox::now_secs;
use crate::mcp::valid_id;
use discover::{
    PAGE_TOKEN_SCOPES, PERMISSION_NOTE, Phone, discover_wabas, granted_scopes, is_permission_code,
    lacks_messaging_task, lacks_whatsapp_scopes, missing_scopes, phone_numbers, phone_status,
    probe_page,
};
use oauth::{
    CallbackError, Dialog, RedirectMode, dialog_url, exchange_code_chain, exchange_fb_token,
    new_state, parse_pasted, read_stdin_line, resolve_redirect, scopes_for, state_matches,
    wait_for_callback,
};
use prompt::Prompter;
use report::{Status, expiry_text, fix, guard, line, say};
use subscribe::{
    PAGE_FIELDS, WHATSAPP_DASHBOARD_STEPS, app_subscriptions, check_callback, page_fields_csv,
    page_subscription, parse_webhook_url, same_url, subscribe_app, subscribe_page, subscribe_waba,
    waba_subscription,
};

const SHORT_LIVED_SECS: i64 = 2 * 3600;
const USER_TOKEN_NOTE: &str = "user tokens expire in 60 days; a System User token (Business settings > System users) can be made non-expiring";

/// Why a command stopped: a bad invocation (exit 2) or a failed step (exit 1).
#[derive(Debug)]
pub enum SetupError {
    Usage(String),
    Fatal { what: String, fix: Option<String> },
}

impl SetupError {
    pub fn usage(message: impl Into<String>) -> Self {
        Self::Usage(message.into())
    }

    pub fn fatal(what: impl Into<String>) -> Self {
        Self::Fatal {
            what: what.into(),
            fix: None,
        }
    }

    pub fn with_fix(self, fix: impl Into<String>) -> Self {
        match self {
            Self::Fatal { what, .. } => Self::Fatal {
                what,
                fix: Some(fix.into()),
            },
            usage => usage,
        }
    }
}

impl From<GraphError> for SetupError {
    fn from(e: GraphError) -> Self {
        Self::fatal(e.to_string())
    }
}

impl From<ConfigError> for SetupError {
    fn from(e: ConfigError) -> Self {
        Self::fatal(e.to_string())
    }
}

/// Logs to stderr; dependency logs stay off because OAuth URLs carry the app secret.
fn init_logging() {
    let mut filter =
        EnvFilter::try_from_env("META_MCP_LOG").unwrap_or_else(|_| EnvFilter::new("meta_mcp=warn"));
    for quiet in [
        "reqwest",
        "hyper",
        "hyper_util",
        "h2",
        "rustls",
        "tokio_rustls",
        "rustls_platform_verifier",
    ] {
        if let Ok(directive) = format!("{quiet}=off").parse() {
            filter = filter.add_directive(directive);
        }
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
}

fn print_error(what: &str, fix_text: Option<&str>) {
    line(Status::Fail, what);
    fix(fix_text.unwrap_or("rerun with META_MCP_LOG=debug for details, or run `meta-mcp doctor`"));
}

fn finish(result: Result<bool, SetupError>) -> ExitCode {
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(SetupError::Usage(message)) => {
            eprintln!("error: {}", report::scrub(&message));
            ExitCode::from(2)
        }
        Err(SetupError::Fatal { what, fix }) => {
            print_error(&what, fix.as_deref());
            ExitCode::from(1)
        }
    }
}

/// What to do about a configuration error; a corrupt credentials file gets the exact `mv`.
pub fn config_fix(e: &ConfigError) -> String {
    match e.0.split_once(" is not valid credentials JSON") {
        Some((path, _)) => format!(
            "move the broken file aside with `mv {} {}.broken`, then rerun `meta-mcp setup`",
            shell_word(path),
            shell_word(path)
        ),
        None => "fix the setting named above and rerun".to_string(),
    }
}

fn load_config() -> Result<Config, SetupError> {
    Config::from_env().map_err(|e| {
        let fix = config_fix(&e);
        SetupError::fatal(format!("configuration problem: {e}")).with_fix(fix)
    })
}

/// The stored credentials of `app_id`; a file without an app id (from `token import`) counts as ours.
fn credentials_of_app<'a>(
    existing: Option<&'a Credentials>,
    app_id: &str,
) -> Option<&'a Credentials> {
    existing.filter(|c| c.app_id.as_deref().is_none_or(|id| id == app_id))
}

/// Registers every secret in a stored file so nothing printed can carry one.
fn guard_credentials(c: &Credentials) {
    let secrets = c
        .app_secret
        .iter()
        .chain(c.user_token.iter().map(|t| &t.token))
        .chain(c.pages.iter().map(|p| &p.token))
        .chain(c.whatsapp.iter().map(|w| &w.token))
        .chain(c.webhook.verify_token.iter());
    for secret in secrets {
        guard(secret);
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// What a run produced; merged into credentials.json without disturbing the rest.
#[derive(Debug, Clone, Default)]
pub struct Produced {
    pub user_token: Option<StoredToken>,
    pub pages: Vec<PageConfig>,
    pub whatsapp: Option<WhatsAppConfig>,
    pub verify_token: Option<Secret>,
    pub public_url: Option<String>,
}

/// Existing credentials plus this run's results: other Pages stay unless `replace_pages` is set.
pub fn merge_credentials(
    existing: Option<Credentials>,
    app_id: &str,
    app_secret: &Secret,
    produced: &Produced,
    replace_pages: bool,
) -> Credentials {
    let mut creds = existing.unwrap_or_default();
    if creds.app_id.as_deref().is_some_and(|old| old != app_id) {
        creds = Credentials::default();
    }
    creds.version = CREDENTIALS_VERSION;
    creds.app_id = Some(app_id.to_string());
    creds.app_secret = Some(app_secret.clone());
    if let Some(t) = &produced.user_token {
        let old = creds.user_token.as_ref().map(|u| u.token.clone());
        if produced.whatsapp.is_none()
            && let Some(wa) = creds.whatsapp.as_mut()
            && wa.token_kind == TokenKind::User
            && Some(&wa.token) == old.as_ref()
        {
            wa.token = t.token.clone();
        }
        creds.user_token = Some(t.clone());
    }
    if replace_pages && !produced.pages.is_empty() {
        creds.pages.clear();
    }
    for page in &produced.pages {
        match creds.pages.iter_mut().find(|p| p.id == page.id) {
            Some(slot) => *slot = page.clone(),
            None => creds.pages.push(page.clone()),
        }
    }
    if let Some(wa) = &produced.whatsapp {
        creds.whatsapp = Some(wa.clone());
    }
    if let Some(v) = &produced.verify_token {
        creds.webhook.verify_token = Some(v.clone());
    }
    if let Some(u) = &produced.public_url {
        creds.webhook.public_url = Some(u.clone());
    }
    creds
}

enum Route {
    Login,
    UserToken(Secret),
    PageToken(Secret),
    Stored,
    NoLogin,
}

/// Which token route a run took, kept after the route's secret has been consumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Login,
    UserToken,
    PageToken,
    Stored,
    NoLogin,
}

impl Route {
    fn source(&self) -> Source {
        match self {
            Self::Login => Source::Login,
            Self::UserToken(_) => Source::UserToken,
            Self::PageToken(_) => Source::PageToken,
            Self::Stored => Source::Stored,
            Self::NoLogin => Source::NoLogin,
        }
    }
}

/// The token route the flags ask for, or `None` when it is left to the default or a prompt.
fn choose_route(args: &SetupArgs) -> Option<Route> {
    let secret = |v: &String| Secret::new(v.trim());
    if let Some(t) = &args.page_token {
        Some(Route::PageToken(secret(t)))
    } else if let Some(t) = &args.user_token {
        Some(Route::UserToken(secret(t)))
    } else if args.login {
        Some(Route::Login)
    } else if args.stored {
        Some(Route::Stored)
    } else if args.no_messenger && args.whatsapp_token.is_some() {
        Some(Route::NoLogin)
    } else {
        None
    }
}

fn check_id(flag: &str, value: &str) -> Result<(), SetupError> {
    if valid_id(value) {
        Ok(())
    } else {
        Err(SetupError::usage(format!(
            "{flag} `{value}` is not a valid id"
        )))
    }
}

/// Rejects bad flag values before anything is asked or sent.
fn validate(args: &SetupArgs) -> Result<(), SetupError> {
    let singles = [
        ("--app-id", args.app_id.as_deref()),
        ("--waba-id", args.waba_id.as_deref()),
        ("--config-id", args.config_id.as_deref()),
    ];
    for (flag, value) in singles {
        if let Some(v) = value {
            check_id(flag, v)?;
        }
    }
    for id in &args.page_ids {
        check_id("--page-id", id)?;
    }
    for id in &args.phone_number_ids {
        check_id("--phone-number-id", id)?;
    }
    resolve_redirect(args.redirect_mode, args.redirect_uri.as_deref(), args.port)
        .map_err(SetupError::usage)?;
    let whatsapp = wants_whatsapp(args);
    if args.no_messenger && !whatsapp && !args.stored {
        return Err(SetupError::usage(
            "--no-messenger leaves nothing to set up: add --whatsapp (or drop --no-messenger)",
        ));
    }
    if let Some(raw) = &args.webhook_url {
        parse_webhook_url(raw).map_err(|e| SetupError::usage(format!("--webhook-url: {e}")))?;
    }
    if args
        .verify_token
        .as_deref()
        .is_some_and(|t| t.trim().is_empty())
    {
        return Err(SetupError::usage("--verify-token cannot be empty"));
    }
    Ok(())
}

/// A run that cannot ask must name its selections up front, before a sign-in is wasted.
fn require_selections(
    args: &SetupArgs,
    interactive: bool,
    stored_whatsapp: bool,
) -> Result<(), SetupError> {
    if interactive {
        return Ok(());
    }
    let signs_in = matches!(
        choose_route(args).unwrap_or(Route::Login),
        Route::Login | Route::UserToken(_)
    );
    if signs_in && !args.no_messenger && args.page_ids.is_empty() && !args.all_pages {
        return Err(SetupError::usage(
            "missing value: pass --page-id ID (repeatable) or --all-pages: the sign-in lists your Pages and a non-interactive run cannot ask which",
        ));
    }
    let reuses_stored = stored_whatsapp && args.waba_id.is_none();
    if wants_whatsapp(args) && args.phone_number_ids.is_empty() && !reuses_stored {
        return Err(SetupError::usage(
            "missing value: pass --phone-number-id ID (repeatable) for the WhatsApp numbers to store",
        ));
    }
    Ok(())
}

fn wants_whatsapp(args: &SetupArgs) -> bool {
    args.whatsapp
        || args.whatsapp_token.is_some()
        || args.waba_id.is_some()
        || !args.phone_number_ids.is_empty()
}

fn resolve_secret(
    args: &SetupArgs,
    prompt: &Prompter,
    app_id: &str,
    existing: Option<&Credentials>,
) -> Result<(Secret, bool), SetupError> {
    if let Some(raw) = &args.app_secret {
        line(
            Status::Warn,
            "--app-secret on the command line stays in your shell history and the process list: prefer --app-secret-env NAME, --app-secret-file PATH or META_APP_SECRET",
        );
        return non_empty_secret(raw, "--app-secret").map(|s| (s, false));
    }
    if let Some(name) = &args.app_secret_env {
        let raw = std::env::var(name).map_err(|_| {
            SetupError::usage(format!(
                "--app-secret-env {name}: that environment variable is not set"
            ))
        })?;
        return non_empty_secret(&raw, "--app-secret-env").map(|s| (s, false));
    }
    if let Some(path) = &args.app_secret_file {
        if let Some(mode) = report::file_mode(path)
            && mode & 0o077 != 0
        {
            line(
                Status::Warn,
                &format!(
                    "{} is readable by other users: chmod 600 it",
                    path.display()
                ),
            );
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|e| SetupError::usage(format!("--app-secret-file {}: {e}", path.display())))?;
        let first = raw
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or_default();
        return non_empty_secret(first, "--app-secret-file").map(|s| (s, false));
    }
    if let Some(from_env) = non_empty_env("META_APP_SECRET") {
        return Ok((Secret::new(from_env), false));
    }
    let stored = existing
        .filter(|c| c.app_id.as_deref() == Some(app_id))
        .and_then(|c| c.app_secret.clone())
        .filter(|s| !s.is_empty());
    match stored {
        Some(secret) => Ok((secret, true)),
        None => prompt
            .secret(
                "--app-secret-env NAME (or META_APP_SECRET)",
                "App Secret (App Dashboard > Settings > Basic)",
            )
            .map(|s| (s, false)),
    }
}

fn non_empty_secret(raw: &str, flag: &str) -> Result<Secret, SetupError> {
    let value = raw.trim();
    if value.is_empty() {
        Err(SetupError::usage(format!(
            "{flag} resolved to an empty secret"
        )))
    } else {
        Ok(Secret::new(value))
    }
}

fn page_label(p: &PageInfo) -> String {
    format!("{} ({})", p.name, p.id)
}

struct Wizard {
    args: SetupArgs,
    cfg: Config,
    prompt: Prompter,
    graph: GraphClient,
    app_id: String,
    app_secret: Secret,
    app_token: Secret,
    existing: Option<Credentials>,
    produced: Produced,
    user_token: Option<StoredToken>,
    messenger: bool,
    whatsapp: bool,
    route: Route,
    webhook_url: Option<Url>,
    source: Source,
    secret_from_store: bool,
    stored_su_dead: bool,
    step_no: usize,
    saved: Option<PathBuf>,
}

/// The `setup` command.
pub async fn run_setup(args: SetupArgs) -> ExitCode {
    init_logging();
    finish(wizard(args).await)
}

async fn wizard(args: SetupArgs) -> Result<bool, SetupError> {
    let prompt = Prompter::new(args.non_interactive, args.yes);
    validate(&args)?;
    let cfg = load_config()?;
    let existing = store::load(&cfg.home)?;
    if let Some(stored) = &existing {
        guard_credentials(stored);
    }
    say("meta-mcp setup");
    let app_id = match args.app_id.clone().or_else(|| cfg.graph.app_id.clone()) {
        Some(id) => id,
        None => prompt.text(
            "--app-id (or META_APP_ID)",
            "Facebook App ID (App Dashboard > Settings > Basic)",
            None,
        )?,
    };
    check_id("--app-id", &app_id)?;
    let (app_secret, secret_from_store) =
        resolve_secret(&args, &prompt, &app_id, existing.as_ref())?;
    let stored_whatsapp = credentials_of_app(existing.as_ref(), &app_id)
        .and_then(|c| c.whatsapp.as_ref())
        .is_some_and(|w| !w.phone_numbers.is_empty());
    require_selections(&args, prompt.is_interactive(), stored_whatsapp)?;
    let app_token = Secret::new(format!("{app_id}|{}", app_secret.expose()));
    guard(&app_secret);
    guard(&app_token);
    let settings = GraphSettings {
        base_url: cfg.graph.base_url.clone(),
        version: cfg.graph.version.clone(),
        app_id: Some(app_id.clone()),
        app_secret: Some(app_secret.clone()),
    };
    let graph = GraphClient::new(&settings, Vec::new())?;
    let mut w = Wizard {
        messenger: !args.no_messenger,
        whatsapp: wants_whatsapp(&args),
        route: Route::Login,
        webhook_url: None,
        source: Source::Login,
        secret_from_store,
        stored_su_dead: false,
        step_no: 0,
        saved: None,
        produced: Produced::default(),
        user_token: None,
        args,
        cfg,
        prompt,
        graph,
        app_id,
        app_secret,
        app_token,
        existing,
    };
    match w.run_steps().await {
        Err(SetupError::Fatal { what, fix }) => {
            print_error(&what, fix.as_deref());
            if let Some(path) = &w.saved {
                line(
                    Status::Info,
                    &format!("progress so far is saved in {}", path.display()),
                );
            }
            Ok(false)
        }
        other => other,
    }
}

impl Wizard {
    fn next_step(&mut self, title: &str) {
        self.step_no += 1;
        report::step(self.step_no, title);
    }

    async fn run_steps(&mut self) -> Result<bool, SetupError> {
        self.step_app().await?;
        self.plan()?;
        self.step_token().await?;
        self.persist_progress()?;
        self.step_pages().await?;
        self.persist_progress()?;
        self.step_whatsapp().await?;
        self.persist_progress()?;
        self.step_webhook().await?;
        self.step_save()?;
        self.step_finish().await
    }

    fn merged(&self) -> Credentials {
        merge_credentials(
            self.existing.clone(),
            &self.app_id,
            &self.app_secret,
            &self.produced,
            self.args.replace,
        )
    }

    fn persist(&mut self) -> Result<PathBuf, SetupError> {
        store::save(&self.cfg.home, &self.merged())?;
        let path = credentials_path(&self.cfg.home);
        self.saved = Some(path.clone());
        Ok(path)
    }

    fn persist_progress(&mut self) -> Result<(), SetupError> {
        let p = &self.produced;
        if p.user_token.is_some() || !p.pages.is_empty() || p.whatsapp.is_some() {
            self.persist()?;
        }
        Ok(())
    }

    /// Swaps in a freshly typed App Secret everywhere the wizard holds one.
    fn replace_app_secret(&mut self, fresh: Secret) -> Result<(), SetupError> {
        guard(&fresh);
        let app_token = Secret::new(format!("{}|{}", self.app_id, fresh.expose()));
        guard(&app_token);
        let settings = GraphSettings {
            base_url: self.cfg.graph.base_url.clone(),
            version: self.cfg.graph.version.clone(),
            app_id: Some(self.app_id.clone()),
            app_secret: Some(fresh.clone()),
        };
        self.graph = GraphClient::new(&settings, Vec::new())?;
        self.app_secret = fresh;
        self.app_token = app_token;
        self.secret_from_store = false;
        Ok(())
    }

    async fn step_app(&mut self) -> Result<(), SetupError> {
        self.next_step("App");
        let query = [("fields", "id,name,link".to_string())];
        let mut outcome = self
            .graph
            .get(&self.app_id, Some(&self.app_token), &query)
            .await;
        let rejected = |r: &Result<Value, GraphError>| {
            r.as_ref()
                .err()
                .is_some_and(|e| matches!(e.code(), Some(190 | 101)))
        };
        if rejected(&outcome) && self.secret_from_store {
            line(
                Status::Warn,
                "the App Secret stored in credentials.json was rejected: was it reset in the App Dashboard?",
            );
            if !self.prompt.is_interactive() {
                return Err(SetupError::fatal(
                    "App ID or App Secret rejected: Settings > Basic in the App Dashboard",
                )
                .with_fix("pass the current secret with --app-secret-env NAME (or META_APP_SECRET), then rerun `meta-mcp setup`"));
            }
            let fresh = self.prompt.secret(
                "--app-secret-env NAME (or META_APP_SECRET)",
                "Current App Secret (App Dashboard > Settings > Basic)",
            )?;
            self.replace_app_secret(fresh)?;
            outcome = self
                .graph
                .get(&self.app_id, Some(&self.app_token), &query)
                .await;
        }
        match outcome {
            Ok(v) => {
                let name = v
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unnamed app");
                line(Status::Ok, &format!("App {name} ({})", self.app_id));
                self.confirm_app_switch()?;
                Ok(())
            }
            Err(e) if matches!(e.code(), Some(190 | 101)) => Err(SetupError::fatal(
                "App ID or App Secret rejected: Settings > Basic in the App Dashboard",
            )
            .with_fix("copy the App ID and the App Secret again (click Show next to the secret), then rerun `meta-mcp setup --app-secret-env NAME`")),
            Err(e) => Err(SetupError::fatal(format!("cannot verify the app: {e}"))
                .with_fix("check the network and that --app-id is the App ID from Settings > Basic")),
        }
    }

    /// Switching apps drops everything stored for the old one, so it needs `--replace`, `--yes` or a yes.
    fn confirm_app_switch(&self) -> Result<(), SetupError> {
        let Some(old) = self.existing.as_ref().and_then(|c| c.app_id.as_deref()) else {
            return Ok(());
        };
        if old == self.app_id {
            return Ok(());
        }
        line(
            Status::Warn,
            &format!(
                "credentials.json belongs to app {old}; this run is for app {}, so its Pages, tokens, WhatsApp and webhook settings would all be dropped",
                self.app_id
            ),
        );
        if self.args.replace || self.prompt.confirm("Replace everything stored?", false) {
            return Ok(());
        }
        Err(SetupError::usage(
            "credentials.json belongs to another app: pass --replace (or --yes) to switch apps, or check --app-id and META_APP_ID",
        ))
    }

    /// The stored credentials, but only when they belong to the app this run is for.
    fn same_app_credentials(&self) -> Option<&Credentials> {
        credentials_of_app(self.existing.as_ref(), &self.app_id)
    }

    /// Scopes the sign-in must grant: WhatsApp ones only when the user token itself serves WhatsApp.
    fn login_scopes(&self) -> Vec<String> {
        let keeps_system_token = self.args.whatsapp_token.is_some()
            || (!self.stored_su_dead
                && self
                    .same_app_credentials()
                    .and_then(|c| c.whatsapp.as_ref())
                    .is_some_and(|w| w.token_kind == TokenKind::SystemUser));
        scopes_for(
            self.messenger,
            self.whatsapp && !keeps_system_token,
            self.args.instagram,
        )
    }

    /// Settles the token route and the optional features before the first sign-in.
    fn plan(&mut self) -> Result<(), SetupError> {
        self.route = match choose_route(&self.args) {
            Some(route) => route,
            None if self.prompt.is_interactive() => self.ask_route()?,
            None => Route::Login,
        };
        let interactive = self.prompt.is_interactive();
        if !self.whatsapp
            && interactive
            && !matches!(self.route, Route::PageToken(_) | Route::NoLogin)
            && self
                .same_app_credentials()
                .is_none_or(|c| c.whatsapp.is_none())
            && self.prompt.offer("Also set up WhatsApp?", false)
        {
            self.whatsapp = true;
        }
        if interactive
            && self.whatsapp
            && self.messenger
            && !matches!(self.route, Route::PageToken(_))
            && self.args.page_ids.is_empty()
            && !self.args.all_pages
            && !self
                .prompt
                .offer("Also set up Messenger (Facebook Pages)?", true)
        {
            self.messenger = false;
        }
        self.webhook_url = self.plan_webhook()?;
        Ok(())
    }

    /// The webhook URL from the flag or environment, or one a person at the keyboard opts into.
    fn plan_webhook(&self) -> Result<Option<Url>, SetupError> {
        let explicit = self
            .args
            .webhook_url
            .clone()
            .or_else(|| non_empty_env("META_WEBHOOK_PUBLIC_URL"));
        let raw = match explicit {
            Some(raw) => Some(raw),
            None if self.prompt.is_interactive() => {
                let stored = self
                    .same_app_credentials()
                    .and_then(|c| c.webhook.public_url.clone());
                match stored {
                    Some(url)
                        if self.prompt.offer(
                            &format!("Refresh the webhook subscription for {url}?"),
                            true,
                        ) =>
                    {
                        Some(url)
                    }
                    Some(_) => None,
                    None if self.prompt.offer(
                        "Subscribe to webhooks now (needs a public https URL that reaches `meta-mcp webhook`)?",
                        false,
                    ) =>
                    {
                        Some(self.prompt.text("--webhook-url", "Public webhook URL", None)?)
                    }
                    None => None,
                }
            }
            None => None,
        };
        raw.map(|r| parse_webhook_url(&r))
            .transpose()
            .map_err(|e| SetupError::usage(format!("webhook URL: {e}")))
    }

    fn ask_route(&self) -> Result<Route, SetupError> {
        let mut options = vec![
            "Sign in with Facebook in the browser (recommended)".to_string(),
            "Paste a user access token".to_string(),
            "Paste a Page access token".to_string(),
        ];
        let has_stored = self
            .same_app_credentials()
            .is_some_and(|c| c.user_token.is_some() || !c.pages.is_empty() || c.whatsapp.is_some());
        if has_stored {
            options.push("Reuse the tokens already stored".to_string());
        }
        let pick = self.prompt.choose_one(
            "--login, --user-token or --page-token",
            "How do you want to sign in?",
            &options,
        )?;
        Ok(match pick {
            0 => Route::Login,
            1 => Route::UserToken(self.prompt.secret("--user-token", "User access token")?),
            2 => Route::PageToken(self.prompt.secret("--page-token", "Page access token")?),
            _ => Route::Stored,
        })
    }

    /// Debug-tokens `tok`; an invalid token stops the run, an unanswered inspection only warns.
    async fn inspect(&self, label: &str, tok: &Secret) -> Result<Option<DebugToken>, SetupError> {
        match token::debug_token(&self.graph, &self.app_token, tok).await {
            Ok(d) if !d.is_valid => Err(SetupError::fatal(format!(
                "Meta reports the {label} as not valid"
            ))
            .with_fix("create a fresh token, or sign in again with `meta-mcp setup --login`")),
            Ok(d) => {
                if !d.app_id.is_empty() && d.app_id != self.app_id {
                    line(
                        Status::Warn,
                        &format!(
                            "the {label} was issued for app {}, not {}",
                            d.app_id, self.app_id
                        ),
                    );
                }
                Ok(Some(d))
            }
            Err(e) if e.code() == Some(190) => Err(SetupError::fatal(format!(
                "the {label} was rejected: {e}"
            ))
            .with_fix("create a fresh token, or sign in again with `meta-mcp setup --login`")),
            Err(e) => {
                line(
                    Status::Warn,
                    &format!("could not inspect the {label} with debug_token: {e}"),
                );
                Ok(None)
            }
        }
    }

    /// Asks Meta whether the stored System User token still works, before a sign-in picks its scopes.
    async fn probe_stored_system_token(&mut self) {
        if !self.whatsapp || self.args.whatsapp_token.is_some() {
            return;
        }
        let Some(stored) = self
            .same_app_credentials()
            .and_then(|c| c.whatsapp.as_ref())
            .filter(|w| w.token_kind == TokenKind::SystemUser)
            .map(|w| w.token.clone())
        else {
            return;
        };
        let dead = match token::debug_token(&self.graph, &self.app_token, &stored).await {
            Ok(d) => !d.is_valid,
            Err(e) => e.code() == Some(190),
        };
        self.stored_su_dead = dead;
    }

    async fn step_token(&mut self) -> Result<(), SetupError> {
        self.probe_stored_system_token().await;
        let route = std::mem::replace(&mut self.route, Route::Login);
        self.source = route.source();
        match route {
            Route::PageToken(tok) => {
                self.next_step("Page token");
                self.token_page(tok).await
            }
            Route::UserToken(tok) => {
                self.next_step("User token");
                self.token_user(tok).await
            }
            Route::Login => {
                self.next_step("Sign in with Facebook");
                self.token_login().await
            }
            Route::Stored => {
                self.next_step("Stored tokens");
                self.token_stored()
            }
            Route::NoLogin => Ok(()),
        }
    }

    async fn token_page(&mut self, tok: Secret) -> Result<(), SetupError> {
        guard(&tok);
        self.user_token = self
            .same_app_credentials()
            .and_then(|c| c.user_token.clone());
        let now = now_secs();
        if let Some(d) = self.inspect("Page token", &tok).await? {
            if let Some(kind) = d.token_type.as_deref().filter(|t| *t != "PAGE") {
                return Err(SetupError::fatal(format!(
                    "this is a {} token, not a Page token",
                    kind.to_lowercase()
                ))
                .with_fix("pass it with --user-token instead: setup then lists your Pages"));
            }
            match d.expires_at {
                None => line(Status::Ok, "Page token is valid, never expires"),
                Some(_) => line(
                    Status::Warn,
                    &format!(
                        "Page token is valid but {}: Page tokens from a sign-in (`meta-mcp setup --login`) do not expire",
                        expiry_text(d.expires_at, now)
                    ),
                ),
            }
            let granted = granted_scopes(&d);
            if !granted.is_empty() {
                for scope in missing_scopes(PAGE_TOKEN_SCOPES, &granted) {
                    line(
                        Status::Warn,
                        &format!(
                            "missing scope {scope} on this Page token: create the token again with it, or run `meta-mcp setup --login --rerequest`"
                        ),
                    );
                }
            }
        }
        let query = [("fields", "id,name,category".to_string())];
        let me = self
            .graph
            .get("me", Some(&tok), &query)
            .await
            .map_err(|e| {
                SetupError::fatal(format!("the Page token was rejected by GET /me: {e}")).with_fix(
                    "create a new Page token (Graph API Explorer, or `meta-mcp setup --login`)",
                )
            })?;
        let id = match me.get("id") {
            Some(Value::String(s)) if valid_id(s) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => {
                return Err(SetupError::fatal("GET /me returned no Page id")
                    .with_fix("make sure this is a Page access token"));
            }
        };
        let name = me
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.produced.pages = vec![PageConfig {
            id,
            name,
            token: tok,
        }];
        Ok(())
    }

    async fn token_user(&mut self, tok: Secret) -> Result<(), SetupError> {
        guard(&tok);
        let now = now_secs();
        let mut token_value = tok;
        let mut expires_at = None;
        let mut scopes = Vec::new();
        let mut inspected = false;
        if let Some(d) = self.inspect("user token", &token_value).await? {
            if d.token_type.as_deref() == Some("PAGE") {
                return Err(SetupError::fatal("this is a Page token, not a user token")
                    .with_fix("pass it with --page-token instead"));
            }
            expires_at = d.expires_at;
            scopes = granted_scopes(&d);
            inspected = true;
        }
        if expires_at.is_some_and(|t| t - now <= SHORT_LIVED_SECS) {
            match token::exchange_long_lived(
                &self.graph,
                &self.app_id,
                &self.app_secret,
                &token_value,
            )
            .await
            {
                Ok(long) => {
                    guard(&long.access_token);
                    line(
                        Status::Ok,
                        "the token was short-lived: exchanged it for a long-lived one (about 60 days)",
                    );
                    token_value = long.access_token;
                    expires_at = long.expires_in.filter(|s| *s > 0).map(|s| now + s);
                    inspected = expires_at.is_some();
                    if let Some(d) = self.inspect("long-lived user token", &token_value).await? {
                        expires_at = d.expires_at.or(expires_at);
                        scopes = granted_scopes(&d);
                        inspected = true;
                    }
                }
                Err(e) => line(
                    Status::Warn,
                    &format!("could not exchange the short-lived token: {e}"),
                ),
            }
        }
        self.finish_user_token(token_value, expires_at, scopes, now, inspected)
    }

    fn finish_user_token(
        &mut self,
        tok: Secret,
        expires_at: Option<i64>,
        scopes: Vec<String>,
        now: i64,
        expiry_known: bool,
    ) -> Result<(), SetupError> {
        let expiry = if expiry_known {
            expiry_text(expires_at, now)
        } else {
            "expiry unknown (debug_token did not answer)".to_string()
        };
        line(Status::Ok, &format!("user token stored, {expiry}"));
        if !scopes.is_empty() {
            line(
                Status::Info,
                &format!("scopes granted: {}", scopes.join(", ")),
            );
            let wanted = self.login_scopes();
            let missing = missing_scopes(wanted.iter().map(String::as_str), &scopes);
            let hint = if self.source == Source::Login {
                "rerun with --rerequest".to_string()
            } else {
                "sign in with `meta-mcp setup --login --rerequest`".to_string()
            };
            for scope in &missing {
                line(Status::Warn, &format!("missing scope {scope}: {hint}"));
            }
            if self.messenger && scopes.iter().all(|s| s != "pages_messaging") {
                return Err(SetupError::fatal("pages_messaging was not granted")
                    .with_fix("rerun `meta-mcp setup --login --rerequest` and accept every permission in the dialog (for WhatsApp only, add --whatsapp --no-messenger)"));
            }
        }
        let stored = StoredToken {
            token: tok,
            expires_at,
            scopes,
            obtained_at: now,
        };
        self.user_token = Some(stored.clone());
        self.produced.user_token = Some(stored);
        Ok(())
    }

    fn token_stored(&mut self) -> Result<(), SetupError> {
        let Some(existing) = self.same_app_credentials() else {
            let other = self.existing.as_ref().is_some_and(|c| c.app_id.is_some());
            return Err(
                SetupError::fatal("there are no stored tokens for this app yet").with_fix(if other {
                    "the stored file belongs to another app: run `meta-mcp setup --login --replace`"
                } else {
                    "run `meta-mcp setup --login` first"
                }),
            );
        };
        if existing.user_token.is_none() && existing.pages.is_empty() && existing.whatsapp.is_none()
        {
            return Err(SetupError::fatal(
                "credentials.json holds no user, Page or WhatsApp token",
            )
            .with_fix("run `meta-mcp setup --login`"));
        }
        let user_token = existing.user_token.clone();
        let summary = format!(
            "using the stored tokens ({} Page(s){})",
            existing.pages.len(),
            if user_token.is_some() {
                ", a user token"
            } else {
                ""
            }
        );
        self.user_token = user_token;
        line(Status::Ok, &summary);
        Ok(())
    }

    async fn token_login(&mut self) -> Result<(), SetupError> {
        if self.login_scopes().is_empty() && self.args.config_id.is_none() {
            line(
                Status::Info,
                "no sign-in is needed: the stored System User token serves WhatsApp and Messenger is off",
            );
            self.user_token = self
                .same_app_credentials()
                .and_then(|c| c.user_token.clone());
            return Ok(());
        }
        let redirect = resolve_redirect(
            self.args.redirect_mode,
            self.args.redirect_uri.as_deref(),
            self.args.port,
        )
        .map_err(SetupError::usage)?;
        let state = new_state();
        let scopes = self.login_scopes();
        let url = dialog_url(&Dialog {
            version: &self.cfg.graph.version,
            app_id: &self.app_id,
            redirect_uri: &redirect.uri,
            state: &state,
            scopes: &scopes,
            config_id: self.args.config_id.as_deref(),
            rerequest: self.args.rerequest,
        });
        let timeout = Duration::from_secs(self.args.timeout_seconds.max(1));
        line(
            Status::Info,
            &format!(
                "redirect URI, register exactly this value: {}",
                redirect.uri
            ),
        );
        say(
            "   Where: App Dashboard > Facebook Login (for Business) > Settings > Valid OAuth Redirect URIs, with Strict Mode on.",
        );
        if redirect.mode == RedirectMode::Loopback && redirect.uri.starts_with("http://") {
            say(
                "   If the dashboard refuses an http:// address (Enforce HTTPS), rerun with --redirect-mode paste.",
            );
        }
        line(
            Status::Info,
            "the person signing in needs a role on the app (Admin, Developer or Tester while it is in Development mode) and the MESSAGING and MODERATE tasks on the Page",
        );
        if self.args.config_id.is_some() {
            line(
                Status::Info,
                "config_id given: the permissions come from that Facebook Login for Business configuration",
            );
        }
        let code = match redirect.mode {
            RedirectMode::Loopback => {
                if std::env::var_os("SSH_CONNECTION").is_some() {
                    line(
                        Status::Info,
                        "this looks like an SSH session: a browser on another machine cannot reach this listener, use --redirect-mode paste",
                    );
                }
                let listener = bind_listener(redirect.port).await?;
                say("Open this URL in your browser to sign in:");
                say(&url);
                self.open_browser(&url);
                line(
                    Status::Info,
                    &format!(
                        "waiting up to {} seconds for the callback on http://127.0.0.1:{}{}",
                        timeout.as_secs(),
                        redirect.port,
                        redirect.path
                    ),
                );
                wait_for_callback(listener, &redirect.path, &state, timeout)
                    .await
                    .map_err(|e| callback_failure(e, &redirect.uri, timeout))?
            }
            RedirectMode::Paste => {
                say("Paste mode: nothing listens on this machine.");
                say(&format!(
                    "After you approve, Facebook sends your browser to {}, which shows a connection error page. That is expected.",
                    redirect.uri
                ));
                say(
                    "Copy the full address from the address bar (it contains ?code=...&state=...) and paste it here.",
                );
                say("Open this URL in your browser to sign in:");
                say(&url);
                self.open_browser(&url);
                say("Paste the address and press Enter:");
                let raw = read_stdin_line(timeout).await.map_err(|e| {
                    SetupError::fatal(format!("no pasted address received: {e}")).with_fix(
                        "rerun `meta-mcp setup` and paste the address bar URL after approving",
                    )
                })?;
                self.code_from_paste(&raw, &state)?
            }
        };
        let long = exchange_code_chain(&self.graph, &self.app_id, &self.app_secret, &redirect.uri, &code)
            .await
            .map_err(|e| {
                SetupError::fatal(format!("exchanging the login code failed: {e}")).with_fix(format!(
                    "the redirect URI must be byte-identical to {} and listed under Valid OAuth Redirect URIs; codes work once, so rerun `meta-mcp setup`",
                    redirect.uri
                ))
            })?;
        line(
            Status::Ok,
            "signed in and exchanged the code for a long-lived user token",
        );
        let now = now_secs();
        let mut expires_at = long.expires_in.filter(|s| *s > 0).map(|s| now + s);
        let mut scopes = Vec::new();
        let mut known = long.expires_in.is_some();
        if let Some(d) = self.inspect("user token", &long.token).await? {
            expires_at = d.expires_at.or(expires_at);
            scopes = granted_scopes(&d);
            known = true;
        }
        self.finish_user_token(long.token, expires_at, scopes, now, known)
    }

    fn code_from_paste(&self, raw: &str, state: &str) -> Result<String, SetupError> {
        let params = parse_pasted(raw).map_err(|e| {
            SetupError::fatal(format!("could not read the pasted text: {e}"))
                .with_fix("paste the whole address bar URL, or just the value after code=")
        })?;
        match &params.state {
            Some(got) if !state_matches(state, got) => {
                return Err(SetupError::fatal(
                    "the state in the pasted URL does not match this sign-in attempt",
                )
                .with_fix("paste the URL from the browser tab opened by this run, or rerun `meta-mcp setup`"));
            }
            None if params.code.is_some() => line(
                Status::Warn,
                "no state in what you pasted: the CSRF check was skipped",
            ),
            _ => {}
        }
        if params.error.is_some() || params.error_reason.is_some() {
            let reason = params
                .error_reason
                .or(params.error)
                .unwrap_or_else(|| "unknown".to_string());
            return Err(SetupError::fatal(format!(
                "Facebook reported `{reason}`: {}",
                params.error_description.unwrap_or_default()
            ))
            .with_fix("rerun `meta-mcp setup` and approve the permissions"));
        }
        params.code.ok_or_else(|| {
            SetupError::fatal("the pasted text carries no code")
                .with_fix("paste the URL that contains ?code=")
        })
    }

    fn open_browser(&self, url: &str) {
        if self.args.no_browser {
            return;
        }
        match open::that_detached(url) {
            Ok(()) => line(Status::Info, "opened your browser"),
            Err(e) => line(
                Status::Warn,
                &format!("could not open a browser ({e}): open the URL above yourself"),
            ),
        }
    }

    async fn step_pages(&mut self) -> Result<(), SetupError> {
        if !self.messenger {
            return Ok(());
        }
        self.next_step("Pages");
        if self.source == Source::PageToken {
            let pages = self.produced.pages.clone();
            self.verify_pages(&pages).await;
            return Ok(());
        }
        if self.source == Source::Stored {
            let stored: Vec<PageConfig> = self
                .same_app_credentials()
                .map(|c| c.pages.clone())
                .unwrap_or_default();
            if let Some(missing) = self
                .args
                .page_ids
                .iter()
                .find(|id| !stored.iter().any(|p| &p.id == *id))
            {
                let known: Vec<&str> = stored.iter().map(|p| p.id.as_str()).collect();
                return Err(SetupError::fatal(format!(
                    "Page {missing} is not stored (stored: {})",
                    known.join(", ")
                ))
                .with_fix("run `meta-mcp setup --login --page-id <id>` to add it"));
            }
            let wanted = &self.args.page_ids;
            let pages: Vec<PageConfig> = stored
                .into_iter()
                .filter(|p| wanted.is_empty() || wanted.contains(&p.id))
                .collect();
            if pages.is_empty() {
                let whatsapp_too = self.whatsapp
                    || self
                        .same_app_credentials()
                        .is_some_and(|c| c.whatsapp.is_some());
                if whatsapp_too {
                    self.messenger = false;
                    line(
                        Status::Info,
                        "no Pages are stored, only WhatsApp: Messenger is skipped",
                    );
                    return Ok(());
                }
                return Err(SetupError::fatal("no Page is stored").with_fix(
                    "run `meta-mcp setup --login --all-pages` (or add --whatsapp for a WhatsApp-only setup)",
                ));
            }
            self.produced.pages = pages.clone();
            self.verify_pages(&pages).await;
            return Ok(());
        }
        let Some(user) = self.user_token.clone() else {
            return Err(SetupError::fatal("no user token to list Pages with")
                .with_fix("sign in with `meta-mcp setup --login`, or pass --page-token"));
        };
        let available = token::accounts(&self.graph, &user.token)
            .await
            .map_err(|e| {
                SetupError::fatal(format!("cannot list your Pages: {e}")).with_fix(
                    "the sign-in needs pages_show_list: rerun `meta-mcp setup --login --rerequest`",
                )
            })?;
        for p in &available {
            guard(&p.access_token);
        }
        if available.is_empty() {
            if self.whatsapp {
                self.messenger = false;
                line(
                    Status::Info,
                    "this login manages no Facebook Page: Messenger is skipped, WhatsApp continues",
                );
                return Ok(());
            }
            return Err(SetupError::fatal("this login manages no Facebook Page")
                .with_fix("the person signing in needs Page access with the MESSAGING task, and must tick the Page in the login dialog (use --rerequest to see it again); for WhatsApp only, add --whatsapp --no-messenger"));
        }
        let picked = self.select_pages(&available)?;
        let mut chosen = Vec::new();
        for index in picked {
            let p = &available[index];
            if p.access_token.is_empty() {
                line(
                    Status::Warn,
                    &format!(
                        "{}: Facebook returned no Page token (the login lacks the MESSAGING or MANAGE task on it)",
                        page_label(p)
                    ),
                );
                continue;
            }
            if lacks_messaging_task(&p.tasks) {
                line(
                    Status::Warn,
                    &format!(
                        "{}: the signed-in person has only the tasks {}; ask a Page admin to add MESSAGING and MODERATE",
                        page_label(p),
                        p.tasks.join(", ")
                    ),
                );
            }
            chosen.push(PageConfig {
                id: p.id.clone(),
                name: p.name.clone(),
                token: p.access_token.clone(),
            });
        }
        if chosen.is_empty() {
            return Err(
                SetupError::fatal("none of the selected Pages came with a token")
                    .with_fix("grant the MESSAGING task on the Page, then rerun with --rerequest"),
            );
        }
        self.produced.pages = chosen.clone();
        self.verify_pages(&chosen).await;
        Ok(())
    }

    fn select_pages(&self, available: &[PageInfo]) -> Result<Vec<usize>, SetupError> {
        if !self.args.page_ids.is_empty() {
            let mut picked = Vec::new();
            for id in &self.args.page_ids {
                let Some(index) = available.iter().position(|p| &p.id == id) else {
                    let known: Vec<String> = available.iter().map(page_label).collect();
                    return Err(SetupError::fatal(format!(
                        "Page {id} is not managed by this login (found: {})",
                        known.join(", ")
                    ))
                    .with_fix("sign in as a person with access to that Page, or pick one of the ids above"));
                };
                if !picked.contains(&index) {
                    picked.push(index);
                }
            }
            return Ok(picked);
        }
        if self.args.all_pages {
            return Ok((0..available.len()).collect());
        }
        if !self.prompt.is_interactive() {
            let known: Vec<String> = available.iter().map(page_label).collect();
            return Err(SetupError::usage(format!(
                "missing value: pass --page-id ID (repeatable) or --all-pages; found: {}",
                known.join(", ")
            )));
        }
        let labels: Vec<String> = available.iter().map(page_label).collect();
        self.prompt.choose(
            "--page-id or --all-pages",
            "Pages this login manages:",
            &labels,
            true,
        )
    }

    async fn verify_pages(&self, pages: &[PageConfig]) {
        for page in pages {
            let probe = probe_page(&self.graph, &page.id, &page.token).await;
            let who = match &probe.name {
                Ok(name) => format!("Page {name} ({})", page.id),
                Err(_) => format!("Page {} ({})", page.name, page.id),
            };
            match (&probe.name, &probe.conversations) {
                (Ok(_), Ok(())) => line(
                    Status::Ok,
                    &format!("{who}: token works and conversations are readable"),
                ),
                (Ok(_), Err(e)) => {
                    line(
                        Status::Warn,
                        &format!("{who}: token works but conversations are not readable: {e}"),
                    );
                    if is_permission_code(e.code()) {
                        say(&format!("   {PERMISSION_NOTE}"));
                    }
                }
                (Err(e), _) => line(Status::Warn, &format!("{who}: cannot read the Page: {e}")),
            }
        }
    }

    async fn step_whatsapp(&mut self) -> Result<(), SetupError> {
        if !self.whatsapp {
            return Ok(());
        }
        self.next_step("WhatsApp");
        let stored = self.same_app_credentials().and_then(|c| c.whatsapp.clone());
        let implicit_stored = self.args.whatsapp_token.is_none()
            && stored
                .as_ref()
                .is_some_and(|w| w.token_kind == TokenKind::SystemUser);
        let (mut wa_token, mut kind) = match (&self.args.whatsapp_token, &stored, &self.user_token)
        {
            (Some(t), _, _) => (Secret::new(t.trim()), TokenKind::SystemUser),
            (None, Some(wa), _) if wa.token_kind == TokenKind::SystemUser => {
                line(
                    Status::Info,
                    "keeping the stored System User token for WhatsApp",
                );
                (wa.token.clone(), TokenKind::SystemUser)
            }
            (None, _, Some(u)) => (u.token.clone(), TokenKind::User),
            (None, _, None) => {
                return Err(SetupError::fatal("WhatsApp needs a user token or a System User token")
                    .with_fix("pass --whatsapp-token <System User token> (with --waba-id and --phone-number-id), or sign in with --login"));
            }
        };
        guard(&wa_token);
        if kind == TokenKind::SystemUser {
            let inspected = self.inspect("WhatsApp token", &wa_token).await;
            if let Ok(Some(d)) = &inspected
                && let Some(t) = d.expires_at
                && t - now_secs() < 7 * 86_400
            {
                line(
                    Status::Warn,
                    &format!(
                        "this WhatsApp token {}: a System User token (Business settings > System users) can be made non-expiring",
                        expiry_text(Some(t), now_secs())
                    ),
                );
            }
            if let Err(e) = inspected {
                let SetupError::Fatal { what, fix } = e else {
                    return Err(e);
                };
                if !implicit_stored {
                    return Err(SetupError::Fatal { what, fix });
                }
                let Some(user) = self.user_token.clone() else {
                    return Err(SetupError::fatal(format!(
                        "the stored System User token no longer works: {what}"
                    ))
                    .with_fix("pass --whatsapp-token <new System User token>, or run `meta-mcp token import --whatsapp-token <token>`"));
                };
                if lacks_whatsapp_scopes(&user.scopes) {
                    return Err(SetupError::fatal(format!(
                        "the stored System User token no longer works ({what}) and your user token has no WhatsApp permissions"
                    ))
                    .with_fix("sign in again with `meta-mcp setup --login --whatsapp --rerequest`, or pass --whatsapp-token <new System User token>"));
                }
                line(
                    Status::Warn,
                    &format!(
                        "the stored System User token no longer works ({what}); using your user token instead"
                    ),
                );
                line(Status::Warn, USER_TOKEN_NOTE);
                wa_token = user.token;
                kind = TokenKind::User;
            }
        } else {
            line(Status::Warn, USER_TOKEN_NOTE);
        }
        if kind == TokenKind::User
            && let Some(user) = &self.user_token
            && lacks_whatsapp_scopes(&user.scopes)
        {
            return Err(SetupError::fatal("your user token has no WhatsApp permissions")
                .with_fix("sign in with `meta-mcp setup --login --whatsapp --rerequest`, or pass --whatsapp-token <System User token>"));
        }
        let known_ids = self.args.waba_id.is_some() && !self.args.phone_number_ids.is_empty();
        let reuse = self.args.phone_number_ids.is_empty() && self.args.waba_id.is_none();
        let reused = stored
            .as_ref()
            .filter(|w| reuse && !w.phone_numbers.is_empty());
        let chosen: Vec<(String, Phone)> = if let Some(wa) = reused {
            line(
                Status::Info,
                "reusing the WhatsApp numbers already stored; pass --phone-number-id to change them",
            );
            wa.phone_numbers
                .iter()
                .map(|p| {
                    (
                        wa.waba_id.clone().unwrap_or_default(),
                        Phone {
                            id: p.id.clone(),
                            display_phone_number: p.display_phone_number.clone(),
                            verified_name: p.verified_name.clone(),
                            ..Phone::default()
                        },
                    )
                })
                .collect()
        } else if kind == TokenKind::SystemUser && known_ids {
            line(
                Status::Info,
                "System User token with --waba-id and --phone-number-id: discovery skipped",
            );
            let waba = self.args.waba_id.clone().unwrap_or_default();
            self.args
                .phone_number_ids
                .iter()
                .map(|id| {
                    (
                        waba.clone(),
                        Phone {
                            id: id.clone(),
                            ..Phone::default()
                        },
                    )
                })
                .collect()
        } else {
            self.discover_numbers(&wa_token, known_ids).await?
        };
        let mut numbers: Vec<PhoneNumber> = Vec::new();
        for (_, phone) in &chosen {
            let status = phone_status(&self.graph, &wa_token, &phone.id)
                .await
                .map_err(|e| {
                    SetupError::fatal(format!("cannot read phone number {}: {e}", phone.id)).with_fix(
                        "check --phone-number-id, and that the token has whatsapp_business_management and access to that account; pass a new System User token with --whatsapp-token if it was revoked",
                    )
                })?;
            let display = if status.display_phone_number.is_empty() {
                phone.display_phone_number.clone()
            } else {
                status.display_phone_number.clone()
            };
            let name = if status.verified_name.is_empty() {
                phone.verified_name.clone()
            } else {
                status.verified_name.clone()
            };
            line(
                Status::Ok,
                &format!(
                    "WhatsApp number {} ({}): {}",
                    display,
                    phone.id,
                    status.summary()
                ),
            );
            numbers.push(PhoneNumber {
                id: phone.id.clone(),
                display_phone_number: display,
                verified_name: name,
            });
        }
        let waba_id = chosen
            .first()
            .map(|(w, _)| w.clone())
            .filter(|w| !w.is_empty());
        if chosen
            .iter()
            .any(|(w, _)| !w.is_empty() && Some(w) != waba_id.as_ref())
        {
            line(
                Status::Warn,
                "the selected numbers belong to several accounts; only the first account id is stored",
            );
        }
        self.produced.whatsapp = Some(WhatsAppConfig {
            waba_id,
            token: wa_token,
            token_kind: kind,
            phone_numbers: numbers,
        });
        Ok(())
    }

    /// Lists accounts and numbers with the token, then applies --waba-id and --phone-number-id or asks.
    async fn discover_numbers(
        &self,
        wa_token: &Secret,
        known_ids: bool,
    ) -> Result<Vec<(String, Phone)>, SetupError> {
        let found = match discover_wabas(&self.graph, wa_token).await {
            Ok(found) => found,
            Err(e) if known_ids => {
                line(
                    Status::Warn,
                    &format!("discovery failed ({e}): using the ids you passed"),
                );
                let waba = self.args.waba_id.clone().unwrap_or_default();
                return Ok(self
                    .args
                    .phone_number_ids
                    .iter()
                    .map(|id| {
                        (
                            waba.clone(),
                            Phone {
                                id: id.clone(),
                                ..Phone::default()
                            },
                        )
                    })
                    .collect());
            }
            Err(e) => {
                return Err(SetupError::fatal(format!("WhatsApp discovery failed: {e}")).with_fix(
                    "the token needs business_management and whatsapp_business_management; sign in again with `meta-mcp setup --login --whatsapp --rerequest`, or pass --whatsapp-token with --waba-id and --phone-number-id",
                ));
            }
        };
        for note in &found.notes {
            line(Status::Warn, &format!("skipped {note}"));
        }
        let wabas: Vec<_> = found
            .wabas
            .iter()
            .filter(|w| self.args.waba_id.as_ref().is_none_or(|id| &w.id == id))
            .collect();
        if wabas.is_empty() {
            return Err(SetupError::fatal(match &self.args.waba_id {
                Some(id) => format!("WhatsApp account {id} is not reachable with this token"),
                None => "no WhatsApp Business Account is reachable with this token".to_string(),
            })
            .with_fix("tick the WhatsApp account in the login dialog (`meta-mcp setup --login --whatsapp --rerequest`), or use a System User token with --whatsapp-token"));
        }
        let mut options: Vec<(String, Phone)> = Vec::new();
        for waba in wabas {
            match phone_numbers(&self.graph, wa_token, &waba.id).await {
                Ok(phones) => {
                    for phone in phones {
                        options.push((waba.id.clone(), phone));
                    }
                }
                Err(e) => line(
                    Status::Warn,
                    &format!(
                        "cannot list the numbers of {} ({}): {e}",
                        waba.name, waba.id
                    ),
                ),
            }
        }
        if options.is_empty() {
            return Err(SetupError::fatal("no WhatsApp phone number was found")
                .with_fix("add a number under App Dashboard > WhatsApp > API Setup, then rerun"));
        }
        let describe = |(waba, p): &(String, Phone)| {
            format!(
                "{} {} [{}] on account {waba}",
                p.display_phone_number, p.verified_name, p.id
            )
        };
        for option in &options {
            line(Status::Info, &describe(option));
        }
        if !self.args.phone_number_ids.is_empty() {
            let mut chosen = Vec::new();
            for id in &self.args.phone_number_ids {
                let Some(found) = options.iter().find(|(_, p)| &p.id == id) else {
                    return Err(SetupError::fatal(format!(
                        "phone number {id} was not found (found: {})",
                        options
                            .iter()
                            .map(|(_, p)| p.id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                    .with_fix("pass one of the ids listed above with --phone-number-id"));
                };
                chosen.push(found.clone());
            }
            return Ok(chosen);
        }
        if !self.prompt.is_interactive() {
            return Err(SetupError::usage(format!(
                "missing value: pass --phone-number-id ID (repeatable); found: {}",
                options
                    .iter()
                    .map(|(_, p)| p.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let labels: Vec<String> = options.iter().map(describe).collect();
        let picked = self
            .prompt
            .choose("--phone-number-id", "Numbers to store:", &labels, true)?;
        Ok(picked.into_iter().map(|i| options[i].clone()).collect())
    }

    async fn step_webhook(&mut self) -> Result<(), SetupError> {
        self.next_step("Webhook");
        let Some(url) = self.webhook_url.clone() else {
            line(
                Status::Info,
                "skipped: pass --webhook-url https://your-host/webhook to subscribe (needed for inbound messages)",
            );
            say(
                "   Later: start `meta-mcp webhook` behind an HTTPS tunnel, then run `meta-mcp setup --stored --webhook-url https://your-host/webhook`.",
            );
            return Ok(());
        };
        let merged = self.merged();
        let pages = self.messenger && !merged.pages.is_empty();
        if !pages && merged.whatsapp.is_none() {
            return Err(SetupError::fatal(
                "there is nothing to subscribe: no Page and no WhatsApp account is configured",
            )
            .with_fix("run `meta-mcp setup --login --all-pages`, or pass --whatsapp-token with --waba-id and --phone-number-id"));
        }
        let (verify, generated) = match self
            .args
            .verify_token
            .as_deref()
            .map(|t| Secret::new(t.trim()))
            .or_else(|| non_empty_env("META_WEBHOOK_VERIFY_TOKEN").map(Secret::new))
            .or_else(|| {
                self.same_app_credentials()
                    .and_then(|c| c.webhook.verify_token.clone())
                    .filter(|t| !t.is_empty())
            }) {
            Some(t) => (t, false),
            None => (Secret::new(random_hex(32)), true),
        };
        guard(&verify);
        self.produced.verify_token = Some(verify.clone());
        self.persist()?;
        if generated {
            line(
                Status::Info,
                "generated a verify token and stored it in credentials.json (it is never printed)",
            );
        }
        say(&format!("Callback URL: {url}"));
        if let Err(why) = check_callback(&url, &verify).await {
            return Err(SetupError::fatal(format!("the callback URL is not reachable: {why}")).with_fix(format!(
                "start `meta-mcp webhook` (it reads the same credentials.json) behind your HTTPS tunnel so that {url} answers, then rerun `{}`",
                self.webhook_rerun(&url)
            )));
        }
        line(
            Status::Ok,
            "callback reachable: the receiver answered the verification handshake",
        );
        let mut reached = false;
        if pages {
            reached = self
                .subscribe_messenger(&url, &verify, &merged.pages)
                .await?
                > 0;
        }
        match &merged.whatsapp {
            Some(wa) => reached |= self.subscribe_whatsapp(&url, &verify, wa).await,
            None if self.args.webhook_override => line(
                Status::Info,
                "--webhook-override has no effect: no WhatsApp account is configured",
            ),
            None => {}
        }
        if !reached {
            return Err(SetupError::fatal(format!(
                "no subscription of this run sends events to {url}, so it was not recorded"
            ))
            .with_fix("rerun with --webhook-override (only the WhatsApp account moves), or --yes (replace the existing callback), or fix the warning above"));
        }
        self.produced.public_url = Some(url.to_string());
        self.persist()?;
        Ok(())
    }

    /// The command that repeats the webhook step on the stored data, with the flags that shaped this run.
    fn webhook_rerun(&self, url: &Url) -> String {
        let mut cmd = format!("meta-mcp setup --stored --webhook-url {url}");
        if !self.messenger {
            cmd.push_str(" --no-messenger");
        }
        if self.args.webhook_override {
            cmd.push_str(" --webhook-override");
        }
        if self.args.yes {
            cmd.push_str(" --yes");
        }
        cmd
    }

    /// Reads what the app sends for `object` today and asks before a different callback is replaced.
    async fn check_replacing(&self, object: &str, url: &Url) -> Result<(), String> {
        let subs = match app_subscriptions(&self.graph, &self.app_token, &self.app_id).await {
            Ok(subs) => subs,
            Err(e) => {
                line(
                    Status::Warn,
                    &format!("cannot read what the app sends for {object} today: {e}"),
                );
                if self
                    .prompt
                    .confirm("Continue without knowing what it replaces?", false)
                {
                    return Ok(());
                }
                return Err(format!(
                    "the current {object} webhook callback could not be read, so it was left alone"
                ));
            }
        };
        let Some(old) = subs
            .iter()
            .find(|s| s.object == object && !s.callback_url.is_empty())
        else {
            return Ok(());
        };
        if same_url(&old.callback_url, url) {
            return Ok(());
        }
        line(
            Status::Warn,
            &format!(
                "the app already sends {object} events to {}; this run replaces that callback",
                old.callback_url
            ),
        );
        if self.prompt.confirm("Replace it?", false) {
            Ok(())
        } else {
            Err(format!(
                "left the existing {object} webhook callback in place"
            ))
        }
    }

    async fn subscribe_messenger(
        &self,
        url: &Url,
        verify: &Secret,
        pages: &[PageConfig],
    ) -> Result<usize, SetupError> {
        self.check_replacing("page", url).await.map_err(|why| {
            SetupError::fatal(why).with_fix(
                "rerun with --yes to replace it, or look at App Dashboard > Webhooks first",
            )
        })?;
        let fields = page_fields_csv();
        subscribe_app(&self.graph, &self.app_token, &self.app_id, "page", url, verify, &fields)
            .await
            .map_err(|e| {
                SetupError::fatal(format!("Meta refused the app-level subscription: {e}")).with_fix(
                    "Meta calls the callback itself: the URL must be reachable from the internet and answer with the same verify token",
                )
            })?;
        match app_subscriptions(&self.graph, &self.app_token, &self.app_id).await {
            Ok(subs) => match subs.iter().find(|s| s.object == "page") {
                Some(s) if !s.active => line(
                    Status::Warn,
                    "app level: Meta lists the object=page subscription as inactive; rerun this command or check the App Dashboard",
                ),
                Some(s) => line(
                    Status::Ok,
                    &format!(
                        "app level: object=page sends {} field(s) to {}: {}",
                        s.fields.len(),
                        s.callback_url,
                        s.fields.join(", ")
                    ),
                ),
                None => line(
                    Status::Warn,
                    "app level: Meta lists no object=page subscription after the update",
                ),
            },
            Err(e) => line(
                Status::Warn,
                &format!("app level: cannot read the subscription back: {e}"),
            ),
        }
        let mut subscribed = 0;
        for page in pages {
            let who = format!("Page {} ({})", page.name, page.id);
            if let Err(e) = subscribe_page(&self.graph, &page.id, &page.token, &fields).await {
                line(Status::Warn, &format!("{who}: subscribing failed: {e}"));
                continue;
            }
            subscribed += 1;
            match page_subscription(&self.graph, &page.id, &page.token, &self.app_id).await {
                Ok(got) => {
                    let missing: Vec<&str> = PAGE_FIELDS
                        .iter()
                        .copied()
                        .filter(|f| !got.iter().any(|g| g == f))
                        .collect();
                    if got.iter().any(|g| g == "messages") && missing.is_empty() {
                        line(
                            Status::Ok,
                            &format!("{who}: subscribed to {}", got.join(", ")),
                        );
                    } else if got.iter().any(|g| g == "messages") {
                        line(
                            Status::Warn,
                            &format!(
                                "{who}: subscribed to {}, not to {}",
                                got.join(", "),
                                missing.join(", ")
                            ),
                        );
                    } else {
                        line(
                            Status::Warn,
                            &format!("{who}: Meta did not keep the messages field"),
                        );
                    }
                }
                Err(e) => line(
                    Status::Warn,
                    &format!("{who}: cannot read the subscription back: {e}"),
                ),
            }
        }
        Ok(subscribed)
    }

    /// True when this account's events now go to `url`; false when nothing was changed or it failed.
    async fn subscribe_whatsapp(&self, url: &Url, verify: &Secret, wa: &WhatsAppConfig) -> bool {
        let Some(waba) = wa.waba_id.as_deref() else {
            line(
                Status::Warn,
                "WhatsApp: no account id is stored, so the account subscription was skipped",
            );
            return false;
        };
        if self.args.webhook_override {
            line(
                Status::Info,
                "--webhook-override: the app-level WhatsApp callback is left untouched",
            );
        } else if !self.subscribe_whatsapp_app(url, verify).await {
            line(
                Status::Info,
                &format!(
                    "WhatsApp account {waba} was left alone; rerun with --webhook-override to send only its events to {url}"
                ),
            );
            return false;
        }
        let over = self.args.webhook_override.then_some((url, verify));
        if let Err(e) = subscribe_waba(&self.graph, waba, &wa.token, over).await {
            line(
                Status::Warn,
                &format!("WhatsApp account {waba}: subscribing failed: {e}"),
            );
            return false;
        }
        match waba_subscription(&self.graph, waba, &wa.token, &self.app_id).await {
            Ok(sub) if !sub.subscribed => line(
                Status::Warn,
                &format!("WhatsApp account {waba}: Meta does not list this app after subscribing"),
            ),
            Ok(sub) => match (&sub.override_uri, self.args.webhook_override) {
                (Some(u), true) if same_url(u, url) => line(
                    Status::Ok,
                    &format!("WhatsApp account {waba}: subscribed with the callback override {u}"),
                ),
                (_, true) => line(
                    Status::Warn,
                    &format!(
                        "WhatsApp account {waba}: subscribed, but Meta does not report the callback override"
                    ),
                ),
                (Some(u), false) if !same_url(u, url) => line(
                    Status::Warn,
                    &format!(
                        "WhatsApp account {waba}: subscribed, but its callback override {u} sends events there instead of {url}"
                    ),
                ),
                (_, false) => line(
                    Status::Ok,
                    &format!("WhatsApp account {waba}: this app is subscribed"),
                ),
            },
            Err(e) => line(
                Status::Warn,
                &format!("WhatsApp account {waba}: cannot read the subscription back: {e}"),
            ),
        }
        true
    }

    /// Meta documents the app-level WhatsApp callback as dashboard-only, so a refusal is expected and explained.
    /// Returns false only when a different live callback was left in place on purpose.
    async fn subscribe_whatsapp_app(&self, url: &Url, verify: &Secret) -> bool {
        if let Err(why) = self.check_replacing("whatsapp_business_account", url).await {
            line(
                Status::Info,
                &format!("{why}; no app-level WhatsApp change was made"),
            );
            return false;
        }
        let sent = subscribe_app(
            &self.graph,
            &self.app_token,
            &self.app_id,
            "whatsapp_business_account",
            url,
            verify,
            "messages",
        )
        .await;
        if let Err(e) = sent {
            line(
                Status::Warn,
                &format!(
                    "Meta did not accept the app-level WhatsApp subscription (WhatsApp webhooks are normally set in the dashboard): {e}"
                ),
            );
            say(&format!(
                "   Configure it by hand: {WHATSAPP_DASHBOARD_STEPS}"
            ));
            say(
                "   Verify token: the value of --verify-token, or webhook.verify_token in credentials.json",
            );
            return true;
        }
        let listed = app_subscriptions(&self.graph, &self.app_token, &self.app_id).await;
        match listed.as_ref().ok().and_then(|subs| {
            subs.iter()
                .find(|s| s.object == "whatsapp_business_account")
        }) {
            Some(s) => line(
                Status::Ok,
                &format!(
                    "app level: object=whatsapp_business_account sends {} to {}",
                    s.fields.join(", "),
                    s.callback_url
                ),
            ),
            None => line(
                Status::Info,
                "app level: Meta accepted the call but does not list WhatsApp objects here; confirm it in the App Dashboard",
            ),
        }
        true
    }

    fn step_save(&mut self) -> Result<(), SetupError> {
        self.next_step("Credentials");
        if self.merged().webhook.verify_token.is_none()
            && non_empty_env("META_WEBHOOK_VERIFY_TOKEN").is_none()
        {
            let created = Secret::new(random_hex(32));
            guard(&created);
            self.produced.verify_token = Some(created);
            line(
                Status::Info,
                "created a webhook verify token in credentials.json (never printed) so `meta-mcp webhook` can start",
            );
        }
        let path = self.persist()?;
        let mode = report::file_mode(&path).map_or_else(|| "n/a".to_string(), |m| format!("{m:o}"));
        line(
            Status::Ok,
            &format!("wrote {} (mode {mode})", path.display()),
        );
        Ok(())
    }

    async fn step_finish(&mut self) -> Result<bool, SetupError> {
        self.next_step("Connect your client and check everything");
        print_client_snippets(self.webhook_url.is_some());
        say("");
        say("Doctor");
        let cfg = load_config()?;
        Ok(doctor::print_report(&cfg, &doctor::Options::default()).await)
    }
}

fn callback_failure(e: CallbackError, redirect_uri: &str, timeout: Duration) -> SetupError {
    match e {
        CallbackError::Timeout { wrong_state } => {
            let refused = if wrong_state > 0 {
                format!(" ({wrong_state} request(s) with a wrong state were refused)")
            } else {
                String::new()
            };
            SetupError::fatal(format!(
                "no callback received within {} seconds{refused}",
                timeout.as_secs()
            ))
            .with_fix(format!(
                "is {redirect_uri} listed under Facebook Login for Business > Settings > Valid OAuth Redirect URIs, with exactly that scheme, host, port and path? Fix it there, or pass --redirect-uri / --port, then rerun"
            ))
        }
        other => SetupError::fatal(format!("the sign-in did not finish: {other}"))
            .with_fix("rerun `meta-mcp setup` and approve the permissions in the browser"),
    }
}

async fn bind_listener(port: u16) -> Result<TcpListener, SetupError> {
    TcpListener::bind(("127.0.0.1", port)).await.map_err(|e| {
        let what = if e.kind() == std::io::ErrorKind::AddrInUse {
            format!("port {port} is already in use")
        } else {
            format!("cannot listen on 127.0.0.1:{port}: {e}")
        };
        SetupError::fatal(what).with_fix(format!(
            "stop the program using port {port}, or choose another with --port <PORT> and register http://localhost:<PORT>/callback under Valid OAuth Redirect URIs (or use --redirect-mode paste)"
        ))
    })
}

/// Quotes `word` for a POSIX shell when it holds anything but plain path characters.
fn shell_word(word: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_./:@%+=,-".contains(c);
    if !word.is_empty() && word.chars().all(plain) {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

fn print_client_snippets(webhook: bool) {
    let home = non_empty_env("META_MCP_HOME");
    say("Claude Code:");
    match &home {
        Some(h) => say(&format!(
            "  claude mcp add meta-mcp --env META_MCP_HOME={} -- meta-mcp",
            shell_word(h)
        )),
        None => say("  claude mcp add meta-mcp -- meta-mcp"),
    }
    say("Any MCP client (JSON):");
    match &home {
        Some(h) => say(&format!(
            "  {}",
            json!({"mcpServers": {"meta-mcp": {"command": "meta-mcp", "env": {"META_MCP_HOME": h}}}})
        )),
        None => say(&format!(
            "  {}",
            json!({"mcpServers": {"meta-mcp": {"command": "meta-mcp"}}})
        )),
    }
    say("HTTP mode (remote clients):");
    say("  META_MCP_API_KEYS=$(openssl rand -hex 32) meta-mcp http");
    if webhook {
        say("Webhook receiver (keep it running behind your HTTPS tunnel):");
        say("  meta-mcp webhook");
    }
    say("Re-check everything any time with `meta-mcp doctor`.");
}

/// The `doctor` command.
pub async fn run_doctor(args: DoctorArgs) -> ExitCode {
    init_logging();
    doctor::run(&args).await
}

/// The `token` command.
pub async fn run_token(args: TokenArgs) -> ExitCode {
    init_logging();
    let result = match args.command {
        TokenCommand::Refresh { whatsapp } => token_refresh(whatsapp).await.map(|()| true),
        TokenCommand::Import(import) => token_import(import).await.map(|()| true),
        TokenCommand::Show { json } => token_show(json).map(|()| true),
    };
    finish(result)
}

struct TokenCtx {
    cfg: Config,
    creds: Credentials,
    graph: GraphClient,
    app_token: Option<Secret>,
}

fn token_ctx() -> Result<TokenCtx, SetupError> {
    let cfg = load_config()?;
    let mut creds = store::load(&cfg.home)?.unwrap_or_default();
    guard_credentials(&creds);
    if creds.app_id.is_none() {
        creds.app_id = cfg.graph.app_id.clone();
    }
    if let (Some(file), Some(configured)) = (&creds.app_id, &cfg.graph.app_id)
        && file != configured
    {
        return Err(SetupError::usage(format!(
            "credentials.json belongs to app {file} but META_APP_ID is {configured}: unset META_APP_ID, or run `meta-mcp setup --app-id {configured} --replace` first"
        )));
    }
    let graph = GraphClient::new(&cfg.graph, Vec::new())?;
    let app_token = cfg.graph.app_token();
    if let Some(t) = &app_token {
        guard(t);
    }
    if let Some(s) = &cfg.graph.app_secret {
        guard(s);
    }
    Ok(TokenCtx {
        cfg,
        creds,
        graph,
        app_token,
    })
}

async fn token_refresh(whatsapp: bool) -> Result<(), SetupError> {
    let mut ctx = token_ctx()?;
    let (Some(app_id), Some(secret)) = (
        ctx.cfg.graph.app_id.clone(),
        ctx.cfg.graph.app_secret.clone(),
    ) else {
        return Err(SetupError::fatal("the App ID and App Secret are not known")
            .with_fix("run `meta-mcp setup`, or set META_APP_ID and META_APP_SECRET"));
    };
    let mut whatsapp_synced = false;
    let mut saved = false;
    let mut user_error = None;
    if let Some(old) = ctx.creds.user_token.clone() {
        match refresh_user(&mut ctx, &app_id, &secret, &old).await {
            Ok(synced) => {
                whatsapp_synced = synced;
                store::save(&ctx.cfg.home, &ctx.creds)?;
                saved = true;
            }
            Err(e) if whatsapp => user_error = Some(e),
            Err(e) => return Err(e),
        }
    } else if !whatsapp {
        return Err(
            SetupError::fatal("no user token is stored, so there is nothing to refresh").with_fix(
                "run `meta-mcp setup --login`, or `meta-mcp token import --user-token <token>`",
            ),
        );
    }
    if whatsapp && !whatsapp_synced {
        match refresh_whatsapp(&mut ctx, &app_id, &secret).await {
            Ok(true) => {
                store::save(&ctx.cfg.home, &ctx.creds)?;
                saved = true;
            }
            Ok(false) => {}
            Err(e) => {
                if saved {
                    line(
                        Status::Info,
                        "the user token and the Page tokens were refreshed and saved before this failure",
                    );
                }
                return match user_error {
                    Some(first) => {
                        if let SetupError::Fatal { what, fix } = &e {
                            print_error(what, fix.as_deref());
                        }
                        Err(first)
                    }
                    None => Err(e),
                };
            }
        }
    }
    if saved {
        line(
            Status::Ok,
            &format!("wrote {}", credentials_path(&ctx.cfg.home).display()),
        );
    }
    match user_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Exchanges the user token and re-derives the Page tokens; true when the WhatsApp user token followed.
async fn refresh_user(
    ctx: &mut TokenCtx,
    app_id: &str,
    secret: &Secret,
    old: &StoredToken,
) -> Result<bool, SetupError> {
    let now = now_secs();
    guard(&old.token);
    let long = exchange_fb_token(&ctx.graph, app_id, secret, &old.token, false)
        .await
        .map_err(|e| {
            let dead = e.code() == Some(190);
            let what = if dead {
                format!(
                    "the user token is no longer valid and an expired token cannot be exchanged: {e}"
                )
            } else {
                format!("refreshing the user token failed: {e}")
            };
            SetupError::fatal(what).with_fix("sign in again with `meta-mcp setup --login`")
        })?;
    let mut expires_at = long.expires_in.filter(|s| *s > 0).map(|s| now + s);
    let mut scopes = old.scopes.clone();
    if let Some(app_token) = &ctx.app_token
        && let Ok(d) = token::debug_token(&ctx.graph, app_token, &long.token).await
        && d.is_valid
    {
        expires_at = d.expires_at.or(expires_at);
        let granted = granted_scopes(&d);
        if !granted.is_empty() {
            scopes = granted;
        }
    }
    line(
        Status::Ok,
        &format!("user token refreshed, {}", expiry_text(expires_at, now)),
    );
    refresh_pages(ctx, &long.token).await;
    ctx.creds.user_token = Some(StoredToken {
        token: long.token.clone(),
        expires_at,
        scopes,
        obtained_at: now,
    });
    let mut synced = false;
    if let Some(wa) = ctx.creds.whatsapp.as_mut()
        && wa.token_kind == TokenKind::User
        && wa.token == old.token
    {
        wa.token = long.token.clone();
        synced = true;
        line(Status::Ok, "WhatsApp token (the user token) replaced");
    }
    Ok(synced)
}

async fn refresh_pages(ctx: &mut TokenCtx, user_token: &Secret) {
    let list = match token::accounts(&ctx.graph, user_token).await {
        Ok(list) => list,
        Err(e) => {
            line(
                Status::Warn,
                &format!("could not re-derive the Page tokens (the old ones were kept): {e}"),
            );
            return;
        }
    };
    for page in ctx.creds.pages.iter_mut() {
        match list
            .iter()
            .find(|p| p.id == page.id && !p.access_token.is_empty())
        {
            Some(fresh) => {
                guard(&fresh.access_token);
                page.token = fresh.access_token.clone();
                if !fresh.name.is_empty() {
                    page.name = fresh.name.clone();
                }
                line(
                    Status::Ok,
                    &format!("Page {} ({}): token replaced", page.name, page.id),
                );
            }
            None => line(
                Status::Warn,
                &format!(
                    "Page {} ({}) was not returned by me/accounts: kept its old token",
                    page.name, page.id
                ),
            ),
        }
    }
}

/// Exchanges the WhatsApp token; `Ok(false)` means it never expires and was left alone.
async fn refresh_whatsapp(
    ctx: &mut TokenCtx,
    app_id: &str,
    secret: &Secret,
) -> Result<bool, SetupError> {
    let Some(wa) = ctx.creds.whatsapp.as_mut() else {
        return Err(SetupError::fatal("WhatsApp is not configured")
            .with_fix("run `meta-mcp setup --whatsapp`"));
    };
    guard(&wa.token);
    if let Some(app_token) = &ctx.app_token {
        match token::debug_token(&ctx.graph, app_token, &wa.token).await {
            Ok(d) if !d.is_valid => {
                return Err(SetupError::fatal(
                    "Meta reports the WhatsApp token as no longer valid, so it cannot be refreshed",
                )
                .with_fix("create a new System User token (Business settings > System users) and run `meta-mcp token import --whatsapp-token <token>`"));
            }
            Ok(d) if d.expires_at.is_none() => {
                line(
                    Status::Ok,
                    "WhatsApp token never expires: nothing to refresh",
                );
                return Ok(false);
            }
            Ok(_) => {}
            Err(e) => {
                return Err(SetupError::fatal(format!(
                    "cannot tell whether the WhatsApp token expires ({e}); refreshing a non-expiring token would make it a 60-day one"
                ))
                .with_fix("rerun `meta-mcp token refresh --whatsapp` once debug_token answers"));
            }
        }
    }
    let sixty = wa.token_kind == TokenKind::SystemUser;
    let fresh = exchange_fb_token(&ctx.graph, app_id, secret, &wa.token, sixty)
        .await
        .map_err(|e| {
            SetupError::fatal(format!("refreshing the WhatsApp token failed: {e}")).with_fix(
                "create a new System User token (Business settings > System users) and run `meta-mcp token import --whatsapp-token <token>`",
            )
        })?;
    wa.token = fresh.token;
    let note = if sixty {
        " (60-day System User token)"
    } else {
        ""
    };
    line(Status::Ok, &format!("WhatsApp token refreshed{note}"));
    Ok(true)
}

/// Checks a token to import with debug_token when the app secret is known.
async fn inspect_import(
    ctx: &TokenCtx,
    tok: &Secret,
    label: &str,
) -> Result<Option<DebugToken>, SetupError> {
    let Some(app_token) = &ctx.app_token else {
        line(
            Status::Info,
            "the App Secret is not known, so the token was not checked with debug_token",
        );
        return Ok(None);
    };
    match token::debug_token(&ctx.graph, app_token, tok).await {
        Ok(d) if !d.is_valid => Err(SetupError::fatal(format!(
            "Meta reports the {label} as not valid"
        ))
        .with_fix("create a fresh token and import that one")),
        Ok(d) => {
            if let Some(app_id) = ctx.cfg.graph.app_id.as_deref()
                && !d.app_id.is_empty()
                && d.app_id != app_id
            {
                line(
                    Status::Warn,
                    &format!("the {label} was issued for app {}, not {app_id}", d.app_id),
                );
            }
            Ok(Some(d))
        }
        Err(e) if e.code() == Some(190) => {
            Err(SetupError::fatal(format!("the {label} was rejected: {e}"))
                .with_fix("create a fresh token and import that one"))
        }
        Err(e) => {
            line(
                Status::Warn,
                &format!("could not inspect the {label} with debug_token: {e}"),
            );
            Ok(None)
        }
    }
}

async fn token_import(args: ImportArgs) -> Result<(), SetupError> {
    args.check().map_err(SetupError::usage)?;
    let mut ctx = token_ctx()?;
    let now = now_secs();
    if let Some(raw) = &args.page_token {
        let tok = Secret::new(raw.trim());
        guard(&tok);
        let d = inspect_import(&ctx, &tok, "Page token").await?;
        if let Some(kind) = d
            .as_ref()
            .and_then(|d| d.token_type.as_deref())
            .filter(|t| *t != "PAGE")
        {
            return Err(SetupError::fatal(format!(
                "this is a {} token, not a Page token",
                kind.to_lowercase()
            ))
            .with_fix("use `meta-mcp token import --user-token <token>` for user tokens"));
        }
        if let Some(d) = &d {
            line(
                Status::Ok,
                &format!("Page token is valid, {}", expiry_text(d.expires_at, now)),
            );
            let granted = granted_scopes(d);
            if !granted.is_empty() {
                for scope in missing_scopes(PAGE_TOKEN_SCOPES, &granted) {
                    line(
                        Status::Warn,
                        &format!("missing scope {scope} on this Page token"),
                    );
                }
            }
        }
        let query = [("fields", "id,name,category".to_string())];
        let me = ctx.graph.get("me", Some(&tok), &query).await.map_err(|e| {
            SetupError::fatal(format!("the Page token was rejected by GET /me: {e}"))
                .with_fix("create a new Page token and import that one")
        })?;
        let id = match me.get("id") {
            Some(Value::String(s)) if valid_id(s) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => {
                return Err(SetupError::fatal("GET /me returned no Page id")
                    .with_fix("make sure this is a Page access token"));
            }
        };
        let name = me
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let page = PageConfig {
            id,
            name,
            token: tok,
        };
        line(
            Status::Ok,
            &format!("Page {} ({}) stored", page.name, page.id),
        );
        match ctx.creds.pages.iter_mut().find(|p| p.id == page.id) {
            Some(slot) => *slot = page,
            None => ctx.creds.pages.push(page),
        }
    } else if let Some(raw) = &args.user_token {
        let tok = Secret::new(raw.trim());
        guard(&tok);
        let d = inspect_import(&ctx, &tok, "user token").await?;
        let me = ctx
            .graph
            .get("me", Some(&tok), &[("fields", "id,name".to_string())])
            .await
            .map_err(|e| {
                SetupError::fatal(format!("the user token was rejected by GET /me: {e}"))
                    .with_fix("create a new token and import that one")
            })?;
        let who = me
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unknown user");
        let expires_at = d.as_ref().and_then(|d| d.expires_at);
        let scopes = d.as_ref().map(granted_scopes).unwrap_or_default();
        let expiry = if d.is_some() {
            expiry_text(expires_at, now)
        } else {
            "expiry unknown (Meta was not asked)".to_string()
        };
        line(
            Status::Ok,
            &format!("user token for {who} stored, {expiry}"),
        );
        if expires_at.is_some_and(|t| t - now <= SHORT_LIVED_SECS) {
            line(
                Status::Info,
                "this looks short-lived: run `meta-mcp token refresh` to extend it to about 60 days",
            );
        }
        let old_user = ctx.creds.user_token.as_ref().map(|u| u.token.clone());
        if let Some(wa) = ctx.creds.whatsapp.as_mut()
            && wa.token_kind == TokenKind::User
            && Some(&wa.token) == old_user.as_ref()
        {
            wa.token = tok.clone();
            line(Status::Ok, "WhatsApp token (the user token) replaced");
        }
        ctx.creds.user_token = Some(StoredToken {
            token: tok,
            expires_at,
            scopes,
            obtained_at: now,
        });
    } else if let Some(raw) = &args.whatsapp_token {
        let tok = Secret::new(raw.trim());
        guard(&tok);
        let d = inspect_import(&ctx, &tok, "WhatsApp token").await?;
        if let Some(d) = &d {
            line(
                Status::Ok,
                &format!(
                    "WhatsApp token is valid, {}",
                    expiry_text(d.expires_at, now)
                ),
            );
        }
        let who = ctx
            .graph
            .get("me", Some(&tok), &[("fields", "id,name".to_string())])
            .await
            .map_err(|e| {
                SetupError::fatal(format!("the WhatsApp token was rejected by GET /me: {e}"))
                    .with_fix("create a new System User token and import that one")
            })?;
        if let Some(name) = who.get("name").and_then(Value::as_str) {
            line(Status::Ok, &format!("the token belongs to {name}"));
        }
        let mut wa = ctx.creds.whatsapp.take().unwrap_or(WhatsAppConfig {
            waba_id: None,
            token: tok.clone(),
            token_kind: TokenKind::SystemUser,
            phone_numbers: Vec::new(),
        });
        wa.token = tok;
        wa.token_kind = TokenKind::SystemUser;
        if let Some(waba) = &args.waba_id {
            check_id("--waba-id", waba)?;
            wa.waba_id = Some(waba.clone());
        }
        if let Some(phone_id) = &args.phone_number_id {
            check_id("--phone-number-id", phone_id)?;
            let status = phone_status(&ctx.graph, &wa.token, phone_id).await.map_err(|e| {
                SetupError::fatal(format!("cannot read phone number {phone_id}: {e}")).with_fix(
                    "check the id, and that the token has whatsapp_business_management and access to that account",
                )
            })?;
            line(
                Status::Ok,
                &format!("WhatsApp number {phone_id}: {}", status.summary()),
            );
            let number = PhoneNumber {
                id: phone_id.clone(),
                display_phone_number: status.display_phone_number,
                verified_name: status.verified_name,
            };
            match wa.phone_numbers.iter_mut().find(|p| &p.id == phone_id) {
                Some(slot) => *slot = number,
                None => wa.phone_numbers.push(number),
            }
        }
        ctx.creds.whatsapp = Some(wa);
    }
    store::save(&ctx.cfg.home, &ctx.creds)?;
    line(
        Status::Ok,
        &format!("wrote {}", credentials_path(&ctx.cfg.home).display()),
    );
    Ok(())
}

/// Everything `token show` reports; it holds no token, secret or verify token value.
fn show_view(cfg: &Config, now: i64) -> Value {
    let path = credentials_path(&cfg.home);
    let user = cfg.user_token.as_ref().map(|t| {
        json!({
            "kind": "user",
            "issued_at": t.obtained_at,
            "issued": report::day(t.obtained_at),
            "expires_at": t.expires_at,
            "days_left": t.expires_at.map(|e| report::days_left(e, now)),
            "scopes": t.scopes,
        })
    });
    let whatsapp = cfg.whatsapp.as_ref().map(|w| {
        json!({
            "token_kind": match w.token_kind {
                TokenKind::User => "user",
                TokenKind::SystemUser => "system_user",
            },
            "waba_id": w.waba_id,
            "phone_numbers": w.phone_numbers.iter().map(|p| json!({
                "id": p.id,
                "display_phone_number": p.display_phone_number,
                "verified_name": p.verified_name,
            })).collect::<Vec<_>>(),
        })
    });
    json!({
        "app_id": cfg.graph.app_id,
        "app_secret_set": cfg.graph.app_secret.is_some(),
        "home": cfg.home.display().to_string(),
        "credentials_file": {
            "path": path.display().to_string(),
            "exists": path.exists(),
            "mode": report::file_mode(&path).map(|m| format!("{m:o}")),
        },
        "user_token": user,
        "pages": cfg.pages.iter().map(|p| json!({"id": p.id, "name": p.name})).collect::<Vec<_>>(),
        "whatsapp": whatsapp,
        "webhook": {
            "public_url": cfg.webhook.public_url.as_ref().map(Url::to_string),
            "verify_token_set": cfg.webhook.verify_token.is_some(),
        },
    })
}

fn token_show(as_json: bool) -> Result<(), SetupError> {
    let cfg = load_config()?;
    let now = now_secs();
    let view = show_view(&cfg, now);
    if as_json {
        say(&serde_json::to_string_pretty(&view).unwrap_or_default());
        return Ok(());
    }
    let app = cfg.graph.app_id.as_deref().unwrap_or("not set");
    let secret = if cfg.graph.app_secret.is_some() {
        "App Secret set"
    } else {
        "App Secret not set"
    };
    say(&format!("App: {app} ({secret})"));
    say(&format!("Home: {}", cfg.home.display()));
    let file = &view["credentials_file"];
    say(&format!(
        "Credentials file: {} ({})",
        file["path"].as_str().unwrap_or_default(),
        match (file["exists"].as_bool(), file["mode"].as_str()) {
            (Some(true), Some(mode)) => format!("mode {mode}"),
            (Some(true), None) => "present".to_string(),
            _ => "missing".to_string(),
        }
    ));
    match &cfg.user_token {
        Some(t) => {
            say(&format!(
                "User token: issued {}, {}",
                report::day(t.obtained_at),
                report::user_expiry_text(t.expires_at, now)
            ));
            if !t.scopes.is_empty() {
                say(&format!("  Scopes: {}", t.scopes.join(", ")));
            }
        }
        None => say("User token: none stored"),
    }
    if cfg.pages.is_empty() {
        say("Pages: none (Page tokens never expire unless revoked)");
    } else {
        say("Pages (Page tokens never expire unless revoked):");
        for p in &cfg.pages {
            say(&format!("  {}  {}", p.id, p.name));
        }
    }
    match &cfg.whatsapp {
        Some(w) => {
            let kind = match w.token_kind {
                TokenKind::User => "user token (expires in about 60 days)",
                TokenKind::SystemUser => "System User token",
            };
            say(&format!(
                "WhatsApp: {kind}, account {}",
                w.waba_id.as_deref().unwrap_or("not set")
            ));
            for p in &w.phone_numbers {
                say(&format!(
                    "  {}  {}  {}",
                    p.id, p.display_phone_number, p.verified_name
                ));
            }
        }
        None => say("WhatsApp: not configured"),
    }
    say(&format!(
        "Webhook: {}, verify token {}",
        cfg.webhook
            .public_url
            .as_ref()
            .map_or("no public URL", Url::as_str),
        if cfg.webhook.verify_token.is_some() {
            "set"
        } else {
            "not set"
        }
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn args(list: &[&str]) -> SetupArgs {
        #[derive(Parser)]
        struct Wrap {
            #[command(flatten)]
            args: SetupArgs,
        }
        Wrap::try_parse_from(std::iter::once("setup").chain(list.iter().copied()))
            .expect("parse")
            .args
    }

    fn page(id: &str, name: &str, token: &str) -> PageConfig {
        PageConfig {
            id: id.into(),
            name: name.into(),
            token: Secret::new(token),
        }
    }

    fn base() -> Credentials {
        Credentials {
            app_id: Some("1".into()),
            app_secret: Some(Secret::new("old-secret")),
            user_token: Some(StoredToken {
                token: Secret::new("EAAold"),
                expires_at: Some(10),
                scopes: vec![],
                obtained_at: 1,
            }),
            pages: vec![page("1", "One", "t1"), page("2", "Two", "t2")],
            whatsapp: Some(WhatsAppConfig {
                waba_id: Some("w".into()),
                token: Secret::new("wa"),
                token_kind: TokenKind::SystemUser,
                phone_numbers: vec![],
            }),
            webhook: store::StoredWebhook {
                verify_token: Some(Secret::new("vt")),
                public_url: Some("https://old.example.org/webhook".into()),
            },
            ..Credentials::default()
        }
    }

    #[test]
    fn merge_keeps_existing_pages_when_adding_one() {
        let produced = Produced {
            pages: vec![page("3", "Three", "t3")],
            ..Produced::default()
        };
        let merged = merge_credentials(
            Some(base()),
            "1",
            &Secret::new("new-secret"),
            &produced,
            false,
        );
        let ids: Vec<&str> = merged.pages.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["1", "2", "3"]);
        assert_eq!(
            merged.app_secret.as_ref().map(Secret::expose),
            Some("new-secret")
        );
        assert_eq!(
            merged.user_token.as_ref().map(|t| t.token.expose()),
            Some("EAAold")
        );
        assert_eq!(
            merged.whatsapp.as_ref().map(|w| w.token.expose()),
            Some("wa")
        );
        assert_eq!(
            merged.webhook.verify_token.as_ref().map(Secret::expose),
            Some("vt")
        );
    }

    #[test]
    fn merge_replaces_a_page_in_place_and_honours_replace() {
        let produced = Produced {
            pages: vec![page("2", "Two renamed", "t2b")],
            ..Produced::default()
        };
        let merged = merge_credentials(Some(base()), "1", &Secret::new("s"), &produced, false);
        assert_eq!(merged.pages.len(), 2);
        assert_eq!(merged.pages[1].name, "Two renamed");
        assert_eq!(merged.pages[1].token.expose(), "t2b");
        let replaced = merge_credentials(Some(base()), "1", &Secret::new("s"), &produced, true);
        assert_eq!(replaced.pages.len(), 1);
        let untouched = merge_credentials(
            Some(base()),
            "1",
            &Secret::new("s"),
            &Produced::default(),
            true,
        );
        assert_eq!(
            untouched.pages.len(),
            2,
            "replace needs pages from this run"
        );
    }

    #[test]
    fn merge_swaps_sections_this_run_produced() {
        let produced = Produced {
            user_token: Some(StoredToken {
                token: Secret::new("EAAnew"),
                expires_at: None,
                scopes: vec!["a".into()],
                obtained_at: 5,
            }),
            verify_token: Some(Secret::new("new-vt")),
            public_url: Some("https://new.example.org/webhook".into()),
            whatsapp: Some(WhatsAppConfig {
                waba_id: Some("w2".into()),
                token: Secret::new("wa2"),
                token_kind: TokenKind::User,
                phone_numbers: vec![],
            }),
            ..Produced::default()
        };
        let merged = merge_credentials(Some(base()), "1", &Secret::new("s"), &produced, false);
        assert_eq!(
            merged.user_token.as_ref().map(|t| t.token.expose()),
            Some("EAAnew")
        );
        assert_eq!(
            merged.whatsapp.as_ref().and_then(|w| w.waba_id.as_deref()),
            Some("w2")
        );
        assert_eq!(
            merged.webhook.verify_token.as_ref().map(Secret::expose),
            Some("new-vt")
        );
        assert_eq!(
            merged.webhook.public_url.as_deref(),
            Some("https://new.example.org/webhook")
        );
        assert_eq!(merged.pages.len(), 2);
    }

    #[test]
    fn merge_moves_a_user_kind_whatsapp_token_along_with_the_user_token() {
        let mut stored = base();
        stored.whatsapp = Some(WhatsAppConfig {
            waba_id: Some("w".into()),
            token: Secret::new("EAAold"),
            token_kind: TokenKind::User,
            phone_numbers: vec![],
        });
        let renewed = Produced {
            user_token: Some(StoredToken {
                token: Secret::new("EAAbrandnew"),
                expires_at: None,
                scopes: vec![],
                obtained_at: 9,
            }),
            ..Produced::default()
        };
        let merged = merge_credentials(
            Some(stored.clone()),
            "1",
            &Secret::new("s"),
            &renewed,
            false,
        );
        assert_eq!(
            merged.whatsapp.as_ref().map(|w| w.token.expose()),
            Some("EAAbrandnew")
        );

        let mut other = stored.clone();
        other.whatsapp.as_mut().expect("whatsapp").token = Secret::new("EAAseparate");
        let merged = merge_credentials(Some(other), "1", &Secret::new("s"), &renewed, false);
        assert_eq!(
            merged.whatsapp.as_ref().map(|w| w.token.expose()),
            Some("EAAseparate"),
            "a different WhatsApp token is left alone"
        );
        let mut system = stored;
        system.whatsapp.as_mut().expect("whatsapp").token_kind = TokenKind::SystemUser;
        let merged = merge_credentials(Some(system), "1", &Secret::new("s"), &renewed, false);
        assert_eq!(
            merged.whatsapp.as_ref().map(|w| w.token.expose()),
            Some("EAAold")
        );
    }

    #[test]
    fn merge_drops_tokens_of_a_different_app() {
        let produced = Produced {
            pages: vec![page("9", "Nine", "t9")],
            ..Produced::default()
        };
        let merged = merge_credentials(Some(base()), "777", &Secret::new("s"), &produced, false);
        assert_eq!(merged.app_id.as_deref(), Some("777"));
        assert_eq!(merged.pages.len(), 1);
        assert!(merged.user_token.is_none() && merged.whatsapp.is_none());
        assert!(merged.webhook.verify_token.is_none());
        let fresh = merge_credentials(None, "5", &Secret::new("s"), &Produced::default(), false);
        assert_eq!(fresh.app_id.as_deref(), Some("5"));
        assert_eq!(fresh.version, CREDENTIALS_VERSION);
    }

    #[test]
    fn shell_words_are_quoted_only_when_needed() {
        assert_eq!(
            shell_word("/home/me/.config/meta-mcp"),
            "/home/me/.config/meta-mcp"
        );
        assert_eq!(shell_word("/tmp/my dir"), "'/tmp/my dir'");
        assert_eq!(shell_word("it's"), "'it'\\''s'");
        assert_eq!(shell_word(""), "''");
    }

    #[test]
    fn route_follows_the_flags() {
        assert!(matches!(
            choose_route(&args(&["--login"])),
            Some(Route::Login)
        ));
        assert!(
            matches!(choose_route(&args(&["--page-token", " EAAp "])), Some(Route::PageToken(t)) if t.expose() == "EAAp")
        );
        assert!(matches!(
            choose_route(&args(&["--user-token", "EAAu"])),
            Some(Route::UserToken(_))
        ));
        assert!(matches!(
            choose_route(&args(&["--stored"])),
            Some(Route::Stored)
        ));
        assert!(matches!(
            choose_route(&args(&["--no-messenger", "--whatsapp-token", "EAAw"])),
            Some(Route::NoLogin)
        ));
        assert!(choose_route(&args(&[])).is_none());
        assert!(choose_route(&args(&["--whatsapp-token", "EAAw"])).is_none());
    }

    #[test]
    fn validation_catches_bad_flag_values_before_any_work() {
        let at_a_terminal = |list: &[&str]| validate(&args(list));
        assert!(
            at_a_terminal(&[
                "--app-id",
                "123",
                "--webhook-url",
                "https://x.example.org/webhook"
            ])
            .is_ok()
        );
        let usage = |list: &[&str]| matches!(at_a_terminal(list), Err(SetupError::Usage(_)));
        assert!(usage(&["--webhook-url", "http://x.example.org/webhook"]));
        assert!(usage(&["--redirect-uri", "ftp://nope"]));
        assert!(usage(&["--page-id", "has space"]));
        assert!(usage(&["--no-messenger"]));
        assert!(usage(&["--no-messenger", "--instagram"]));
        assert!(usage(&["--verify-token", "  "]));
        assert!(at_a_terminal(&["--no-messenger", "--whatsapp"]).is_ok());
        assert!(at_a_terminal(&["--whatsapp", "--webhook-override"]).is_ok());
    }

    #[test]
    fn non_interactive_runs_must_name_their_selections_up_front() {
        let check = |list: &[&str]| require_selections(&args(list), false, false);
        let missing = |list: &[&str], flag: &str| matches!(check(list), Err(SetupError::Usage(m)) if m.contains(flag));
        assert!(missing(&[], "--all-pages"));
        assert!(missing(&["--login"], "--page-id"));
        assert!(missing(&["--user-token", "EAAu"], "--all-pages"));
        assert!(check(&["--login", "--all-pages"]).is_ok());
        assert!(check(&["--user-token", "EAAu", "--page-id", "42"]).is_ok());
        assert!(
            check(&["--page-token", "EAAp"]).is_ok(),
            "a Page token needs no pick"
        );
        assert!(check(&["--stored"]).is_ok(), "stored Pages need no pick");
        assert!(missing(
            &["--user-token", "EAAu", "--no-messenger", "--whatsapp"],
            "--phone-number-id"
        ));
        assert!(
            check(&[
                "--user-token",
                "EAAu",
                "--no-messenger",
                "--whatsapp",
                "--phone-number-id",
                "P1"
            ])
            .is_ok()
        );
        assert!(
            check(&[
                "--no-messenger",
                "--whatsapp-token",
                "EAAw",
                "--waba-id",
                "W1",
                "--phone-number-id",
                "P1"
            ])
            .is_ok(),
            "a System User token needs no login"
        );
        assert!(
            require_selections(&args(&["--login"]), true, false).is_ok(),
            "a terminal can ask"
        );
        let stored = |list: &[&str]| require_selections(&args(list), false, true);
        assert!(
            stored(&["--stored", "--whatsapp"]).is_ok(),
            "stored numbers are reused"
        );
        assert!(
            matches!(
                stored(&["--stored", "--whatsapp", "--waba-id", "W1"]),
                Err(SetupError::Usage(m)) if m.contains("--phone-number-id")
            ),
            "a new account id needs its numbers"
        );
        assert!(
            validate(&args(&["--stored", "--no-messenger"])).is_ok(),
            "WhatsApp-only stored credentials"
        );
    }

    #[test]
    fn corrupt_credentials_get_the_exact_move_command() {
        let broken = ConfigError(
            "/home/me/.config/meta-mcp/credentials.json is not valid credentials JSON (EOF); fix it or run `meta-mcp setup` again".into(),
        );
        let fix = config_fix(&broken);
        assert!(
            fix.contains("mv /home/me/.config/meta-mcp/credentials.json /home/me/.config/meta-mcp/credentials.json.broken"),
            "{fix}"
        );
        let spaced =
            ConfigError("/tmp/my dir/credentials.json is not valid credentials JSON (x)".into());
        assert!(config_fix(&spaced).contains("'/tmp/my dir/credentials.json'"));
        assert_eq!(
            config_fix(&ConfigError("META_MCP_PORT must be a port number".into())),
            "fix the setting named above and rerun"
        );
    }

    #[test]
    fn show_view_never_holds_a_token_or_secret() {
        let dir = tempfile::tempdir().expect("tmp");
        store::save(dir.path(), &base()).expect("save");
        let cfg = Config::from_lookup(|_| None, Some(dir.path().to_path_buf())).expect("config");
        let view = show_view(&cfg, 100).to_string();
        for secret in ["old-secret", "EAAold", "t1", "t2", "\"wa\"", "vt\""] {
            assert!(!view.contains(secret), "{secret} leaked in {view}");
        }
        assert!(
            view.contains("\"id\":\"1\"") && view.contains("system_user"),
            "{view}"
        );
    }
}
