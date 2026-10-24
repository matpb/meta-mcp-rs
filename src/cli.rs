//! Command line: argument parsing and dispatch. Exit codes: 0 ok, 1 runtime or config error, 2 usage.

use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use crate::config::Config;
use crate::{mcp, server};

/// MCP server for the Meta Graph API (Facebook Messenger and WhatsApp Cloud API).
#[derive(Debug, Parser)]
#[command(name = "meta-mcp", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Arguments of commands that arrive in a later phase; accepted so scripts fail clearly.
#[derive(Debug, Args)]
pub struct Pending {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
    pub args: Vec<String>,
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
    /// Guided setup (arrives in the next phase)
    Setup(Pending),
    /// Check the whole configuration against Meta (arrives in the next phase)
    Doctor(Pending),
    /// Refresh, show or import tokens (arrives in the next phase)
    Token(Pending),
    /// Print every tool name, one per line, honouring read-only mode
    Tools,
}

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::from(1)
}

fn pending(name: &str) -> ExitCode {
    eprintln!("{name} arrives in the next phase");
    ExitCode::from(2)
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

async fn execute(command: Command) -> ExitCode {
    match command {
        Command::Setup(_) => return pending("setup"),
        Command::Doctor(_) => return pending("doctor"),
        Command::Token(_) => return pending("token"),
        _ => {}
    }
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
        Command::Setup(_) | Command::Doctor(_) | Command::Token(_) => Ok(()),
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
    fn pending_commands_accept_future_flags() {
        for args in [
            &["setup", "--webhook-url", "https://x.example/webhook"][..],
            &["setup"],
            &["doctor", "--json"],
            &["token", "refresh"],
        ] {
            assert!(parse(args).is_ok(), "{args:?}");
        }
    }

    #[test]
    fn bad_usage_is_an_error() {
        assert!(parse(&["bogus"]).is_err());
        assert!(parse(&["http", "--nope"]).is_err());
        assert!(parse(&["stdio", "extra"]).is_err());
    }
}
