//! macOS: a launchd agent in `~/Library/LaunchAgents` that starts the daemon at login.

use std::path::{Path, PathBuf};

pub const LABEL: &str = "dev.meta-mcp.personal";

fn xml_escape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

fn string_entry(out: &mut String, indent: &str, key: &str, value: &str) {
    out.push_str(&format!(
        "{indent}<key>{}</key>\n{indent}<string>{}</string>\n",
        xml_escape(key),
        xml_escape(value)
    ));
}

/// `$HOME/Library/LaunchAgents/dev.meta-mcp.personal.plist`.
pub fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

/// `$HOME/Library/Logs/meta-mcp/daemon.log`.
pub fn log_path(home: &Path) -> PathBuf {
    home.join("Library/Logs/meta-mcp/daemon.log")
}

/// The agent's property list: runs `<exe> personal daemon` at load, restarts it after a failure only.
pub fn render_plist(exe: &Path, env: &[(String, String)], log: &Path) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n",
    );
    string_entry(&mut out, "  ", "Label", LABEL);
    out.push_str("  <key>ProgramArguments</key>\n  <array>\n");
    for arg in [
        exe.display().to_string(),
        "personal".into(),
        "daemon".into(),
    ] {
        out.push_str(&format!("    <string>{}</string>\n", xml_escape(&arg)));
    }
    out.push_str("  </array>\n");
    if !env.is_empty() {
        out.push_str("  <key>EnvironmentVariables</key>\n  <dict>\n");
        for (k, v) in env {
            string_entry(&mut out, "    ", k, v);
        }
        out.push_str("  </dict>\n");
    }
    out.push_str("  <key>RunAtLoad</key>\n  <true/>\n");
    out.push_str("  <key>KeepAlive</key>\n  <dict>\n    <key>SuccessfulExit</key>\n    <false/>\n  </dict>\n");
    out.push_str("  <key>ThrottleInterval</key>\n  <integer>10</integer>\n");
    out.push_str("  <key>Umask</key>\n  <integer>63</integer>\n");
    string_entry(&mut out, "  ", "ProcessType", "Background");
    let log = log.display().to_string();
    string_entry(&mut out, "  ", "StandardOutPath", &log);
    string_entry(&mut out, "  ", "StandardErrorPath", &log);
    out.push_str("</dict>\n</plist>\n");
    out
}

/// The three launchctl lines a dry run lists, for the user id `uid`.
fn install_commands(plist: &Path, uid: &str) -> [String; 3] {
    [
        format!("launchctl bootout gui/{uid}/{LABEL}"),
        format!("launchctl bootstrap gui/{uid} {}", plist.display()),
        format!("launchctl kickstart gui/{uid}/{LABEL}"),
    ]
}

/// The whole `install --dry-run` block (without the sidecar warnings).
pub fn render_install_dry_run(plist_path: &Path, plist: &str, uid: &str) -> String {
    let mut out = format!("# {}\n{plist}\n", plist_path.display());
    out.push_str("# commands (dry run: nothing written, nothing run)\n");
    for line in install_commands(plist_path, uid) {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The whole `uninstall --dry-run` block.
pub fn render_uninstall_dry_run(plist_path: &Path, uid: &str) -> String {
    format!(
        "launchctl bootout gui/{uid}/{LABEL}\nremove {}\n(dry run: nothing removed, nothing run; sessions and the audit log are kept)\n",
        plist_path.display()
    )
}

#[cfg(target_os = "macos")]
pub use run::{install, uninstall};

#[cfg(target_os = "macos")]
mod run {
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode};
    use std::time::Duration;

    use super::{
        LABEL, log_path, plist_path, render_install_dry_run, render_plist, render_uninstall_dry_run,
    };
    use crate::personal::cli::install::{
        missing_sidecars, non_empty_env, run_shown, service_env, wait_for_daemon, write_unit,
    };
    use crate::personal::{ENV_SOCKET, ipc, secure};

    const BOOTSTRAP_TRIES: u32 = 5;
    const BOOTSTRAP_PAUSE: Duration = Duration::from_millis(500);
    const READY_WAIT: Duration = Duration::from_secs(10);

    struct Plan {
        plist_path: PathBuf,
        plist: String,
        log: PathBuf,
        /// The home and the inbox folder, made before the daemon starts.
        create: Vec<PathBuf>,
        /// The files folder, which the user must have made.
        must_exist: Vec<PathBuf>,
    }

    fn user_home() -> Result<PathBuf, String> {
        non_empty_env("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "cannot find the home folder: set HOME".to_owned())
    }

    fn plan() -> Result<Plan, String> {
        let exe = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(|e| format!("cannot resolve the meta-mcp executable path: {e}"))?;
        let mut env = service_env(&non_empty_env)?;
        if let Some(socket) = non_empty_env(ENV_SOCKET) {
            if !socket.starts_with('/') || socket.chars().any(char::is_control) {
                return Err(format!("{ENV_SOCKET} must be an absolute path"));
            }
            env.push((ENV_SOCKET.to_owned(), socket));
        }
        let home = user_home()?;
        let log = log_path(&home);
        let value = |name: &str| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
        let create = value("META_MCP_HOME")
            .map(PathBuf::from)
            .into_iter()
            .chain(
                value("META_MCP_INBOX_DB")
                    .and_then(|v| Path::new(v).parent())
                    .map(Path::to_path_buf),
            )
            .collect();
        let must_exist = value("META_MCP_FILES_DIR")
            .map(PathBuf::from)
            .into_iter()
            .collect();
        Ok(Plan {
            plist_path: plist_path(&home),
            plist: render_plist(&exe, &env, &log),
            log,
            create,
            must_exist,
        })
    }

    fn launchctl(args: &[&str]) -> bool {
        let shown = format!("launchctl {}", args.join(" "));
        let mut cmd = Command::new("launchctl");
        cmd.args(args);
        run_shown(cmd, "launchctl", &shown)
    }

    pub async fn install(dry_run: bool) -> ExitCode {
        let plan = match plan() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        };
        let uid = match ipc::own_identity().await {
            Ok(u) => u.to_string(),
            Err(e) => {
                eprintln!("error: cannot read the user id: {e}");
                return ExitCode::FAILURE;
            }
        };
        let missing = missing_sidecars();
        if dry_run {
            for m in &missing {
                println!("warning: {m}");
            }
            print!(
                "{}",
                render_install_dry_run(&plan.plist_path, &plan.plist, &uid)
            );
            return ExitCode::SUCCESS;
        }
        if !missing.is_empty() {
            eprintln!("error: the sidecar binaries cannot be located:");
            for m in &missing {
                eprintln!("  {m}");
            }
            eprintln!(
                "Put them next to meta-mcp or set META_MCP_SIDECAR_DIR, then run install again."
            );
            return ExitCode::FAILURE;
        }
        for dir in &plan.must_exist {
            if !dir.is_dir() {
                eprintln!(
                    "error: {} does not exist: create it first (META_MCP_FILES_DIR)",
                    dir.display()
                );
                return ExitCode::FAILURE;
            }
        }
        let log_dir = plan.log.parent().map(Path::to_path_buf);
        for dir in plan.create.iter().chain(log_dir.iter()) {
            if let Err(e) = secure::private_dir(dir) {
                eprintln!("error: cannot create {}: {e}", dir.display());
                return ExitCode::FAILURE;
            }
        }
        let Some(agents) = plan.plist_path.parent() else {
            eprintln!("error: the agent path has no folder");
            return ExitCode::FAILURE;
        };
        let name = format!("{LABEL}.plist");
        let written =
            std::fs::create_dir_all(agents).and_then(|()| write_unit(agents, &name, &plan.plist));
        if let Err(e) = written {
            eprintln!("error: cannot write {}: {e}", plan.plist_path.display());
            return ExitCode::FAILURE;
        }
        println!("Wrote {}", plan.plist_path.display());
        let target = format!("gui/{uid}/{LABEL}");
        let domain = format!("gui/{uid}");
        let plist_arg = plan.plist_path.display().to_string();
        launchctl(&["bootout", &target]);
        let mut bootstrapped = false;
        for attempt in 0..BOOTSTRAP_TRIES {
            if launchctl(&["bootstrap", &domain, &plist_arg]) {
                bootstrapped = true;
                break;
            }
            if attempt + 1 < BOOTSTRAP_TRIES {
                tokio::time::sleep(BOOTSTRAP_PAUSE).await;
            }
        }
        if !bootstrapped || !launchctl(&["kickstart", &target]) {
            eprintln!(
                "error: launchd did not accept the agent; see {}",
                plan.log.display()
            );
            return ExitCode::FAILURE;
        }
        if wait_for_daemon(READY_WAIT).await {
            println!(
                "The daemon runs from login. Next: meta-mcp personal login messenger | whatsapp"
            );
            ExitCode::SUCCESS
        } else {
            eprintln!(
                "error: the daemon did not answer within 10 s; see {}",
                plan.log.display()
            );
            ExitCode::FAILURE
        }
    }

    pub async fn uninstall(dry_run: bool) -> ExitCode {
        let home = match user_home() {
            Ok(h) => h,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        };
        let plist = plist_path(&home);
        let uid = match ipc::own_identity().await {
            Ok(u) => u.to_string(),
            Err(e) => {
                eprintln!("error: cannot read the user id: {e}");
                return ExitCode::FAILURE;
            }
        };
        if dry_run {
            print!("{}", render_uninstall_dry_run(&plist, &uid));
            return ExitCode::SUCCESS;
        }
        let target = format!("gui/{uid}/{LABEL}");
        let loaded = Command::new("launchctl")
            .args(["print", &target])
            .output()
            .is_ok_and(|o| o.status.success());
        let mut ok = true;
        if loaded {
            ok &= launchctl(&["bootout", &target]);
        }
        match std::fs::remove_file(&plist) {
            Ok(()) => println!("removed {}", plist.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                eprintln!("error: cannot remove {}: {e}", plist.display());
                ok = false;
            }
        }
        println!(
            "Kept the stored sessions, state and audit log under the meta-mcp home (personal/)."
        );
        if ok {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Vec<(String, String)> {
        vec![
            ("META_MCP_HOME".into(), "/Users/a b/meta".into()),
            ("META_MCP_PERSONAL_SOCKET".into(), "/tmp/x&y.sock".into()),
        ]
    }

    #[test]
    fn the_plist_runs_the_daemon_at_login_and_restarts_only_after_a_failure() {
        let log = Path::new("/Users/a/Library/Logs/meta-mcp/daemon.log");
        let p = render_plist(Path::new("/usr/local/bin/meta-mcp"), &env(), log);
        for needle in [
            "<key>Label</key>\n  <string>dev.meta-mcp.personal</string>",
            "<string>/usr/local/bin/meta-mcp</string>\n    <string>personal</string>\n    <string>daemon</string>",
            "<key>META_MCP_HOME</key>\n    <string>/Users/a b/meta</string>",
            "<key>META_MCP_PERSONAL_SOCKET</key>\n    <string>/tmp/x&amp;y.sock</string>",
            "<key>RunAtLoad</key>\n  <true/>",
            "<key>KeepAlive</key>\n  <dict>\n    <key>SuccessfulExit</key>\n    <false/>",
            "<key>ThrottleInterval</key>\n  <integer>10</integer>",
            "<key>Umask</key>\n  <integer>63</integer>",
            "<key>ProcessType</key>\n  <string>Background</string>",
            "<key>StandardOutPath</key>\n  <string>/Users/a/Library/Logs/meta-mcp/daemon.log</string>",
            "<key>StandardErrorPath</key>\n  <string>/Users/a/Library/Logs/meta-mcp/daemon.log</string>",
        ] {
            assert!(p.contains(needle), "{needle}\n{p}");
        }
        assert!(p.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist"));
        assert!(p.ends_with("</dict>\n</plist>\n"));
    }

    #[test]
    fn special_characters_are_escaped_in_every_string() {
        let env = vec![("K&<".to_owned(), "a&b<c>d\"e'f".to_owned())];
        let p = render_plist(Path::new("/x/<exe>&"), &env, Path::new("/l/\"log\""));
        assert!(p.contains("<key>K&amp;&lt;</key>"), "{p}");
        assert!(
            p.contains("<string>a&amp;b&lt;c&gt;d&quot;e&apos;f</string>"),
            "{p}"
        );
        assert!(p.contains("<string>/x/&lt;exe&gt;&amp;</string>"), "{p}");
        assert!(p.contains("<string>/l/&quot;log&quot;</string>"), "{p}");
    }

    #[test]
    fn paths_follow_the_library_layout() {
        let home = Path::new("/Users/a");
        assert_eq!(
            plist_path(home),
            PathBuf::from("/Users/a/Library/LaunchAgents/dev.meta-mcp.personal.plist")
        );
        assert_eq!(
            log_path(home),
            PathBuf::from("/Users/a/Library/Logs/meta-mcp/daemon.log")
        );
    }

    #[test]
    fn the_dry_runs_list_exactly_the_planned_steps() {
        let plist = plist_path(Path::new("/Users/a"));
        let shown = plist.display();
        assert_eq!(
            render_install_dry_run(&plist, "<plist/>", "501"),
            format!(
                "# {shown}\n<plist/>\n# commands (dry run: nothing written, nothing run)\nlaunchctl bootout gui/501/dev.meta-mcp.personal\nlaunchctl bootstrap gui/501 {shown}\nlaunchctl kickstart gui/501/dev.meta-mcp.personal\n"
            )
        );
        assert_eq!(
            render_uninstall_dry_run(&plist, "501"),
            format!(
                "launchctl bootout gui/501/dev.meta-mcp.personal\nremove {shown}\n(dry run: nothing removed, nothing run; sessions and the audit log are kept)\n"
            )
        );
    }
}
