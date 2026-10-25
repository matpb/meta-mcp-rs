//! Command line: argument parsing and dispatch. Exit codes: 0 ok, 1 runtime or config error, 2 usage.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{ArgGroup, Args, Parser, Subcommand};

use crate::config::Config;
#[cfg(unix)]
use crate::personal::PersonalPlatform;
use crate::setup::oauth::RedirectMode;
use crate::{mcp, server, setup};

/// MCP server for Facebook Messenger and WhatsApp: the Meta Graph API (Pages and business numbers) and, on Unix, personal accounts through local sidecars.
#[derive(Debug, Parser)]
#[command(name = "meta-mcp", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Options of `meta-mcp setup`.
#[derive(Debug, Clone, Args)]
#[command(
    after_help = "Every value comes from its flag, then the environment (META_APP_ID, META_APP_SECRET, META_WEBHOOK_PUBLIC_URL, META_WEBHOOK_VERIFY_TOKEN), then credentials.json, then a prompt. With --non-interactive, or when stdin is not a terminal, a missing value is a usage error (exit 2) that names the flag. The login URL is printed in full; secrets never are."
)]
pub struct SetupArgs {
    /// Facebook App ID (or META_APP_ID)
    #[arg(long, value_name = "ID")]
    pub app_id: Option<String>,
    /// App Secret (it stays in shell history: prefer --app-secret-env or META_APP_SECRET)
    #[arg(long, value_name = "SECRET", conflicts_with_all = ["app_secret_env", "app_secret_file"])]
    pub app_secret: Option<String>,
    /// Read the App Secret from this environment variable
    #[arg(long, value_name = "NAME", conflicts_with = "app_secret_file")]
    pub app_secret_env: Option<String>,
    /// Read the App Secret from the first line of this file
    #[arg(long, value_name = "PATH")]
    pub app_secret_file: Option<PathBuf>,

    /// Sign in with Facebook Login in the browser (the default)
    #[arg(long, conflicts_with_all = ["user_token", "page_token", "stored"])]
    pub login: bool,
    /// Use this user access token instead of signing in
    #[arg(long, value_name = "TOKEN", conflicts_with_all = ["login", "page_token", "stored"])]
    pub user_token: Option<String>,
    /// Use this Page access token (no user token is stored)
    #[arg(long, value_name = "TOKEN", conflicts_with_all = ["login", "user_token", "stored", "no_messenger"])]
    pub page_token: Option<String>,
    /// Reuse the tokens already in credentials.json instead of signing in
    #[arg(long, conflicts_with_all = ["login", "user_token", "page_token"])]
    pub stored: bool,

    /// How the login code returns: a loopback listener or a pasted URL
    #[arg(long, value_enum, default_value_t = RedirectMode::Loopback)]
    pub redirect_mode: RedirectMode,
    /// Redirect URI to send to Facebook (must match Valid OAuth Redirect URIs exactly)
    #[arg(long, value_name = "URI")]
    pub redirect_uri: Option<String>,
    /// Loopback listener port (default 8766)
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,
    /// Facebook Login for Business configuration id (replaces the scope list)
    #[arg(long, value_name = "ID")]
    pub config_id: Option<String>,
    /// Ask again for permissions that were declined before
    #[arg(long)]
    pub rerequest: bool,
    /// Print the login URL but do not open a browser
    #[arg(long)]
    pub no_browser: bool,

    /// Page to store (repeatable)
    #[arg(long = "page-id", value_name = "ID", conflicts_with_all = ["all_pages", "no_messenger"])]
    pub page_ids: Vec<String>,
    /// Store every Page the login can manage
    #[arg(long, conflicts_with = "no_messenger")]
    pub all_pages: bool,
    /// Skip Messenger and Pages (WhatsApp only)
    #[arg(long)]
    pub no_messenger: bool,
    /// Drop stored Pages this run did not select; also allows switching to another app
    #[arg(long)]
    pub replace: bool,

    /// Also set up the WhatsApp Cloud API
    #[arg(long)]
    pub whatsapp: bool,
    /// System User token for WhatsApp (otherwise the user token is used)
    #[arg(long, value_name = "TOKEN")]
    pub whatsapp_token: Option<String>,
    /// WhatsApp Business Account id
    #[arg(long, value_name = "ID")]
    pub waba_id: Option<String>,
    /// WhatsApp phone number id to store (repeatable)
    #[arg(long = "phone-number-id", value_name = "ID")]
    pub phone_number_ids: Vec<String>,
    /// Also request the Instagram messaging permissions
    #[arg(long)]
    pub instagram: bool,

    /// Public https URL of the webhook receiver (or META_WEBHOOK_PUBLIC_URL)
    #[arg(long, value_name = "URL")]
    pub webhook_url: Option<String>,
    /// Point only the WhatsApp account at the webhook and leave the app-level callback alone
    #[arg(long)]
    pub webhook_override: bool,
    /// Webhook verify token (or META_WEBHOOK_VERIFY_TOKEN); generated when absent
    #[arg(long, value_name = "TOKEN")]
    pub verify_token: Option<String>,

    /// Never prompt: a missing value is a usage error (exit 2)
    #[arg(long)]
    pub non_interactive: bool,
    /// Seconds to wait for the login callback or the pasted URL
    #[arg(long, value_name = "SECONDS", default_value_t = 300)]
    pub timeout_seconds: u64,
    /// Answer yes to confirmations: replacing a different webhook callback, switching apps
    #[arg(short = 'y', long)]
    pub yes: bool,
}

/// Options of `meta-mcp doctor`.
#[derive(Debug, Clone, Args)]
pub struct DoctorArgs {
    /// Print one JSON document instead of the text report
    #[arg(long)]
    pub json: bool,
    /// Also test that the public webhook URL answers Meta's verification handshake
    #[arg(long)]
    pub webhook: bool,
}

/// The token to store with `meta-mcp token import`; exactly one kind per run.
#[derive(Debug, Clone, Args)]
#[command(group(ArgGroup::new("kind").required(true).multiple(false).args(["page_token", "user_token", "whatsapp_token"])))]
pub struct ImportArgs {
    /// A Page access token
    #[arg(long, value_name = "TOKEN")]
    pub page_token: Option<String>,
    /// A user access token
    #[arg(long, value_name = "TOKEN")]
    pub user_token: Option<String>,
    /// A WhatsApp (System User) access token
    #[arg(long, value_name = "TOKEN")]
    pub whatsapp_token: Option<String>,
    /// Phone number id to store with the WhatsApp token
    #[arg(long, value_name = "ID")]
    pub phone_number_id: Option<String>,
    /// WhatsApp Business Account id to store with the WhatsApp token
    #[arg(long, value_name = "ID")]
    pub waba_id: Option<String>,
}

impl ImportArgs {
    /// The WhatsApp ids only make sense with a WhatsApp token (clap cannot express this next to the group).
    pub fn check(&self) -> Result<(), String> {
        if self.whatsapp_token.is_none()
            && (self.phone_number_id.is_some() || self.waba_id.is_some())
        {
            return Err("--phone-number-id and --waba-id go with --whatsapp-token".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Subcommand)]
pub enum TokenCommand {
    /// Exchange the stored user token for a fresh one and re-derive the Page tokens
    Refresh {
        /// Refresh the WhatsApp token instead of leaving it alone
        #[arg(long)]
        whatsapp: bool,
    },
    /// Validate a token with Meta and store it
    Import(ImportArgs),
    /// Show what is stored: kinds, expiry, scopes, Pages and numbers, never a token
    Show {
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },
}

#[cfg(unix)]
/// The personal platforms a command can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum PersonalPlatformArg {
    Messenger,
    Whatsapp,
}

#[cfg(unix)]
impl PersonalPlatformArg {
    fn platform(self) -> PersonalPlatform {
        match self {
            Self::Messenger => PersonalPlatform::Messenger,
            Self::Whatsapp => PersonalPlatform::WhatsApp,
        }
    }
}

#[cfg(unix)]
/// A personal-account login flow.
#[derive(Debug, Clone, Subcommand)]
pub enum LoginCommand {
    /// Log in to Messenger with the cookies of a browser session
    Messenger {
        /// File holding a cURL command, a JSON object of cookies or a JSON array of {name, value}
        #[arg(long, value_name = "PATH")]
        cookies_file: Option<PathBuf>,
    },
    /// Link this machine to WhatsApp as a companion device (QR code, or a code typed on the phone)
    Whatsapp {
        /// Link with a code typed on the phone instead of a QR code: the phone number, digits only
        #[arg(long, value_name = "PHONE")]
        pair_code: Option<String>,
        /// Ask the phone for the full message history (honoured on a first link only)
        #[arg(long)]
        full_history: bool,
        /// Name shown in the phone's list of linked devices
        #[arg(long, value_name = "NAME", default_value = "meta-mcp")]
        device_name: String,
    },
}

#[cfg(unix)]
/// A personal-account command.
#[derive(Debug, Clone, Subcommand)]
pub enum PersonalCommand {
    /// Run the long-lived daemon that owns the Messenger and WhatsApp sidecars
    Daemon {
        /// Unix socket to listen on (default: $XDG_RUNTIME_DIR/meta-mcp/personal.sock)
        #[arg(long, value_name = "PATH")]
        socket: Option<PathBuf>,
    },
    /// Log in to a personal account through the daemon
    #[command(subcommand)]
    Login(LoginCommand),
    /// Show both accounts: login, connection, sending state, versions
    Status {
        /// Print the daemon's answer as JSON
        #[arg(long)]
        json: bool,
    },
    /// Forget the stored session of an account
    Logout {
        #[arg(value_enum)]
        platform: PersonalPlatformArg,
    },
    /// Restart an account's sidecar and clear a sending block
    Restart {
        #[arg(value_enum)]
        platform: PersonalPlatformArg,
    },
    /// Install and start the daemon as systemd user units
    Install {
        /// Print the units and the commands without writing or running anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Stop the daemon and remove its systemd user units (sessions and audit log stay)
    Uninstall {
        /// Print what would happen without doing it
        #[arg(long)]
        dry_run: bool,
    },
}

#[cfg(unix)]
/// Options of `meta-mcp personal`.
#[derive(Debug, Clone, Args)]
pub struct PersonalArgs {
    #[command(subcommand)]
    pub command: PersonalCommand,
}

/// Options of `meta-mcp personal` on a system without personal mode: the words are ignored.
#[cfg(not(unix))]
#[derive(Debug, Clone, Args)]
pub struct PersonalArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
    pub rest: Vec<String>,
}

/// Options of `meta-mcp token`.
#[derive(Debug, Clone, Args)]
pub struct TokenArgs {
    #[command(subcommand)]
    pub command: TokenCommand,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Serve MCP over stdin and stdout (the default)
    Stdio,
    /// Serve MCP over streamable HTTP at /mcp (needs META_MCP_API_KEYS)
    Http {
        /// Also mount the webhook receiver at /webhook on the same listener
        #[arg(long)]
        with_webhook: bool,
    },
    /// Run only the webhook receiver that feeds the local inbox
    Webhook,
    /// Guided setup: sign in, pick Pages and numbers, subscribe webhooks, write credentials
    Setup(Box<SetupArgs>),
    /// Check the whole configuration against Meta and say what to fix
    Doctor(DoctorArgs),
    /// Refresh, import or show tokens
    Token(TokenArgs),
    /// Personal Messenger and WhatsApp accounts through the local daemon
    Personal(PersonalArgs),
    /// Print every tool name, one per line, honouring read-only mode
    Tools,
}

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::from(1)
}

/// Parses the command line, runs the chosen command and returns the process exit code.
pub async fn run() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let code = e.exit_code();
            let _ = e.print();
            return ExitCode::from(u8::try_from(code).unwrap_or(2));
        }
    };
    execute(cli.command.unwrap_or(Command::Stdio)).await
}

#[cfg(not(unix))]
async fn run_personal(_args: PersonalArgs) -> ExitCode {
    eprintln!("personal mode needs a Unix system");
    ExitCode::from(2)
}

#[cfg(unix)]
async fn run_personal(args: PersonalArgs) -> ExitCode {
    use crate::personal::cli as pcli;
    match args.command {
        PersonalCommand::Daemon { socket } => {
            crate::personal::daemon::run(crate::personal::daemon::DaemonOptions { socket }).await
        }
        PersonalCommand::Login(LoginCommand::Messenger { cookies_file }) => {
            pcli::login_messenger(cookies_file).await
        }
        PersonalCommand::Login(LoginCommand::Whatsapp {
            pair_code,
            full_history,
            device_name,
        }) => {
            pcli::login_whatsapp(pcli::WhatsAppLogin {
                pair_code,
                full_history,
                device_name,
            })
            .await
        }
        PersonalCommand::Status { json } => pcli::status(json).await,
        PersonalCommand::Logout { platform } => pcli::logout(platform.platform()).await,
        PersonalCommand::Restart { platform } => pcli::restart(platform.platform()).await,
        PersonalCommand::Install { dry_run } => pcli::install(dry_run),
        PersonalCommand::Uninstall { dry_run } => pcli::uninstall(dry_run),
    }
}

async fn execute(command: Command) -> ExitCode {
    let command = match command {
        Command::Setup(args) => return setup::run_setup(*args).await,
        Command::Doctor(args) => return setup::run_doctor(args).await,
        Command::Token(args) => return setup::run_token(args).await,
        Command::Personal(args) => return run_personal(args).await,
        other => other,
    };
    server::init_tracing();
    let cfg = match Config::from_env() {
        Ok(cfg) => cfg,
        Err(e) => return fail(e),
    };
    let result = match command {
        Command::Tools => {
            for name in mcp::tool_names_for(cfg.read_only) {
                println!("{name}");
            }
            Ok(())
        }
        Command::Stdio => server::run_stdio(cfg).await,
        Command::Http { with_webhook } => server::run_http(cfg, with_webhook).await,
        Command::Webhook => server::run_webhook(cfg).await,
        Command::Setup(_) | Command::Doctor(_) | Command::Token(_) | Command::Personal(_) => Ok(()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(format!("{e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("meta-mcp").chain(args.iter().copied()))
    }

    #[cfg(unix)]
    #[test]
    fn personal_daemon_parses_with_an_optional_socket() {
        assert!(matches!(
            parse(&["personal", "daemon"]).expect("daemon").command,
            Some(Command::Personal(PersonalArgs {
                command: PersonalCommand::Daemon { socket: None }
            }))
        ));
        let cli = parse(&["personal", "daemon", "--socket", "/tmp/x.sock"]).expect("socket");
        let Some(Command::Personal(PersonalArgs {
            command: PersonalCommand::Daemon { socket },
        })) = cli.command
        else {
            panic!("not a personal daemon command");
        };
        assert_eq!(socket, Some(PathBuf::from("/tmp/x.sock")));
        assert!(parse(&["personal"]).is_err());
    }

    #[test]
    fn subcommands_parse() {
        assert!(parse(&[]).expect("none").command.is_none());
        assert!(matches!(
            parse(&["stdio"]).expect("stdio").command,
            Some(Command::Stdio)
        ));
        assert!(matches!(
            parse(&["http"]).expect("http").command,
            Some(Command::Http {
                with_webhook: false
            })
        ));
        assert!(matches!(
            parse(&["http", "--with-webhook"]).expect("http").command,
            Some(Command::Http { with_webhook: true })
        ));
        assert!(matches!(
            parse(&["webhook"]).expect("webhook").command,
            Some(Command::Webhook)
        ));
        assert!(matches!(
            parse(&["tools"]).expect("tools").command,
            Some(Command::Tools)
        ));
    }

    #[test]
    fn setup_doctor_and_token_arguments_parse() {
        for args in [
            &["setup", "--webhook-url", "https://x.example/webhook"][..],
            &["setup"],
            &[
                "setup",
                "--app-id",
                "1",
                "--app-secret-env",
                "MY_SECRET",
                "--login",
                "--no-browser",
            ],
            &[
                "setup",
                "--page-token",
                "EAAx",
                "--non-interactive",
                "--yes",
            ],
            &[
                "setup",
                "--redirect-mode",
                "paste",
                "--page-id",
                "1",
                "--page-id",
                "2",
            ],
            &[
                "setup",
                "--user-token",
                "EAAx",
                "--whatsapp",
                "--no-messenger",
                "--phone-number-id",
                "P1",
                "--phone-number-id",
                "P2",
            ],
            &["doctor"],
            &["doctor", "--json", "--webhook"],
            &["token", "refresh"],
            &["token", "refresh", "--whatsapp"],
            &["token", "show", "--json"],
            &["token", "import", "--page-token", "EAAx"],
            &[
                "token",
                "import",
                "--whatsapp-token",
                "EAAx",
                "--phone-number-id",
                "P1",
                "--waba-id",
                "W1",
            ],
        ] {
            assert!(parse(args).is_ok(), "{args:?}");
        }
        let Some(Command::Setup(a)) = parse(&["setup", "--port", "9000", "--timeout-seconds", "5"])
            .expect("setup")
            .command
        else {
            panic!("not setup");
        };
        assert_eq!((a.port, a.timeout_seconds), (Some(9000), 5));
        assert_eq!(a.redirect_mode, RedirectMode::Loopback);
    }

    #[test]
    fn conflicting_setup_flags_are_rejected() {
        for args in [
            &["setup", "--login", "--page-token", "EAAx"][..],
            &["setup", "--user-token", "a", "--page-token", "b"],
            &["setup", "--app-secret", "a", "--app-secret-env", "B"],
            &["setup", "--all-pages", "--page-id", "1"],
            &["setup", "--no-messenger", "--all-pages"],
            &["token", "import"],
            &["token", "import", "--page-token", "a", "--user-token", "b"],
            &["token"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn whatsapp_ids_need_the_whatsapp_token() {
        let import = |args: &[&str]| match parse(args).expect("parse").command {
            Some(Command::Token(TokenArgs {
                command: TokenCommand::Import(i),
            })) => i,
            other => panic!("not an import: {other:?}"),
        };
        assert!(
            import(&[
                "token",
                "import",
                "--page-token",
                "a",
                "--phone-number-id",
                "P"
            ])
            .check()
            .is_err()
        );
        assert!(
            import(&["token", "import", "--user-token", "a", "--waba-id", "W"])
                .check()
                .is_err()
        );
        let ok = import(&[
            "token",
            "import",
            "--whatsapp-token",
            "a",
            "--phone-number-id",
            "P",
            "--waba-id",
            "W",
        ]);
        assert!(ok.check().is_ok());
        assert!(
            import(&["token", "import", "--page-token", "a"])
                .check()
                .is_ok()
        );
    }

    #[test]
    fn bad_usage_is_an_error() {
        assert!(parse(&["bogus"]).is_err());
        assert!(parse(&["http", "--nope"]).is_err());
        assert!(parse(&["stdio", "extra"]).is_err());
    }
}
