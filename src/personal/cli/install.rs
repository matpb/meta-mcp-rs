//! `personal install` and `personal uninstall`: systemd user units for the daemon.

use std::ffi::OsString;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::personal::{ENV_SOCKET, PersonalPlatform, sidecar};

pub const SOCKET_UNIT: &str = "meta-mcp-personal.socket";
pub const SERVICE_UNIT: &str = "meta-mcp-personal.service";
const DEFAULT_LISTEN: &str = "%t/meta-mcp/personal.sock";
/// Variables copied into the service when the installing shell sets them; the bool marks paths.
const PASSED_ENV: [(&str, bool); 5] = [
    ("META_MCP_INBOX_DB", true),
    ("META_MCP_FILES_DIR", true),
    ("META_MCP_SIDECAR_DIR", true),
    ("META_MCP_READ_ONLY", false),
    ("META_MCP_LOG", false),
];

/// The two unit files as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Units {
    pub socket: String,
    pub service: String,
}

fn escape_value(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
}

/// A command word for `ExecStart=`: quoted only when it holds characters systemd would split on.
fn exec_word(v: &str) -> String {
    let plain = !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+:,@=".contains(c));
    if plain {
        return v.to_owned();
    }
    let body = v
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "$$")
        .replace('%', "%%");
    format!("\"{body}\"")
}

fn listen_value(v: &str) -> String {
    v.replace('%', "%%")
}

/// Renders both units. `listen` is the socket path to bind (None: the runtime directory default).
pub fn render_units(exe: &Path, listen: Option<&str>, env: &[(String, String)]) -> Units {
    let listen = listen.map_or_else(|| DEFAULT_LISTEN.to_owned(), listen_value);
    let socket = format!(
        "[Unit]\nDescription=meta-mcp personal daemon socket\n\n[Socket]\nListenStream={listen}\nSocketMode=0600\nDirectoryMode=0700\nRemoveOnStop=yes\n\n[Install]\nWantedBy=sockets.target\n"
    );
    let mut service = format!(
        "[Unit]\nDescription=meta-mcp personal daemon (Messenger and WhatsApp)\nRequires={SOCKET_UNIT}\nAfter={SOCKET_UNIT}\n\n[Service]\nType=simple\nExecStart={} personal daemon\n",
        exec_word(&exe.display().to_string())
    );
    for (k, v) in env {
        service.push_str(&format!("Environment=\"{k}={}\"\n", escape_value(v)));
    }
    service.push_str(
        "Restart=on-failure\nRestartSec=5\nRestartPreventExitStatus=2\nUMask=0077\nNoNewPrivileges=yes\nTimeoutStopSec=20\n\n[Install]\nWantedBy=default.target\n",
    );
    Units { socket, service }
}

/// `$XDG_CONFIG_HOME/systemd/user`, else `<home>/.config/systemd/user`.
pub fn unit_dir(xdg: Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    let xdg = xdg
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let base = xdg.or_else(|| home.map(|h| h.join(".config")))?;
    Some(base.join("systemd/user"))
}

fn absolute(p: &str) -> String {
    std::path::absolute(p).map_or_else(|_| p.to_owned(), |a| a.display().to_string())
}

/// The `Environment=` pairs for the service: the home always, the rest when set here.
fn service_env(get: &dyn Fn(&str) -> Option<String>) -> Result<Vec<(String, String)>, String> {
    let home = match get("META_MCP_HOME") {
        Some(h) => absolute(&h),
        None => crate::config::default_home()
            .map_err(|e| format!("cannot determine the meta-mcp home: {e}"))?
            .display()
            .to_string(),
    };
    let mut env = vec![("META_MCP_HOME".to_owned(), home)];
    for (name, is_path) in PASSED_ENV {
        if let Some(v) = get(name) {
            env.push((name.to_owned(), if is_path { absolute(&v) } else { v }));
        }
    }
    for (k, v) in &env {
        if v.chars().any(char::is_control) {
            return Err(format!(
                "{k} holds a control character and cannot go in a unit"
            ));
        }
    }
    Ok(env)
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

struct Plan {
    dir: PathBuf,
    units: Units,
}

fn plan() -> Result<Plan, String> {
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("cannot resolve the meta-mcp executable path: {e}"))?;
    let listen = non_empty_env(ENV_SOCKET);
    if let Some(l) = &listen
        && (!l.starts_with('/') || l.chars().any(|c| c.is_whitespace() || c.is_control()))
    {
        return Err(format!(
            "{ENV_SOCKET} must be an absolute path without whitespace to go in a unit"
        ));
    }
    let env = service_env(&non_empty_env)?;
    let dir = unit_dir(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME").map(PathBuf::from),
    )
    .ok_or("cannot find the systemd user unit directory: set XDG_CONFIG_HOME or HOME")?;
    Ok(Plan {
        dir,
        units: render_units(&exe, listen.as_deref(), &env),
    })
}

/// The sidecar binaries that are not where the daemon will look for them.
fn missing_sidecars() -> Vec<String> {
    PersonalPlatform::ALL
        .iter()
        .filter_map(|p| match sidecar::locate(*p) {
            Ok(path) if path.is_file() => None,
            Ok(path) => Some(format!(
                "{} not found at {}",
                p.binary_name(),
                path.display()
            )),
            Err(e) => Some(e.to_string()),
        })
        .collect()
}

fn write_unit(dir: &Path, name: &str, text: &str) -> std::io::Result<()> {
    let path = dir.join(name);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(&path)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
}

/// Runs `systemctl --user <args>` and reports its exit status; true when it exited 0.
fn systemctl(args: &[&str]) -> bool {
    let shown = format!("systemctl --user {}", args.join(" "));
    match Command::new("systemctl").arg("--user").args(args).status() {
        Ok(s) if s.success() => {
            println!("ok (exit 0): {shown}");
            true
        }
        Ok(s) => {
            println!(
                "failed (exit {}): {shown}",
                s.code().map_or("signal".into(), |c| c.to_string())
            );
            false
        }
        Err(e) => {
            println!("failed (cannot run systemctl: {e}): {shown}");
            false
        }
    }
}

pub fn install(dry_run: bool) -> ExitCode {
    let plan = match plan() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let missing = missing_sidecars();
    if !missing.is_empty() && !dry_run {
        eprintln!("error: the sidecar binaries cannot be located:");
        for m in &missing {
            eprintln!("  {m}");
        }
        eprintln!("Put them next to meta-mcp or set META_MCP_SIDECAR_DIR, then run install again.");
        return ExitCode::FAILURE;
    }
    if dry_run {
        for m in &missing {
            println!("warning: {m}");
        }
        println!("# {}", plan.dir.join(SOCKET_UNIT).display());
        println!("{}", plan.units.socket);
        println!("# {}", plan.dir.join(SERVICE_UNIT).display());
        println!("{}", plan.units.service);
        println!("# commands (dry run: nothing written, nothing run)");
        println!("systemctl --user daemon-reload");
        println!("systemctl --user enable --now {SOCKET_UNIT} {SERVICE_UNIT}");
        return ExitCode::SUCCESS;
    }
    let written = std::fs::create_dir_all(&plan.dir)
        .and_then(|()| write_unit(&plan.dir, SOCKET_UNIT, &plan.units.socket))
        .and_then(|()| write_unit(&plan.dir, SERVICE_UNIT, &plan.units.service));
    if let Err(e) = written {
        eprintln!(
            "error: cannot write the units in {}: {e}",
            plan.dir.display()
        );
        return ExitCode::FAILURE;
    }
    println!(
        "Wrote {SOCKET_UNIT} and {SERVICE_UNIT} in {}",
        plan.dir.display()
    );
    let ok =
        systemctl(&["daemon-reload"]) && systemctl(&["enable", "--now", SOCKET_UNIT, SERVICE_UNIT]);
    if !ok {
        eprintln!(
            "error: systemd did not accept the units; see `journalctl --user -u {SERVICE_UNIT}`"
        );
        return ExitCode::FAILURE;
    }
    println!("The daemon runs from login. Next: meta-mcp personal login messenger | whatsapp");
    ExitCode::SUCCESS
}

pub fn uninstall(dry_run: bool) -> ExitCode {
    let Some(dir) = unit_dir(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME").map(PathBuf::from),
    ) else {
        eprintln!(
            "error: cannot find the systemd user unit directory: set XDG_CONFIG_HOME or HOME"
        );
        return ExitCode::FAILURE;
    };
    if dry_run {
        println!("systemctl --user disable --now {SERVICE_UNIT} {SOCKET_UNIT}");
        println!("remove {}", dir.join(SERVICE_UNIT).display());
        println!("remove {}", dir.join(SOCKET_UNIT).display());
        println!("systemctl --user daemon-reload");
        println!("(dry run: nothing removed, nothing run; sessions and the audit log are kept)");
        return ExitCode::SUCCESS;
    }
    let mut ok = systemctl(&["disable", "--now", SERVICE_UNIT, SOCKET_UNIT]);
    for name in [SERVICE_UNIT, SOCKET_UNIT] {
        match std::fs::remove_file(dir.join(name)) {
            Ok(()) => println!("removed {}", dir.join(name).display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                eprintln!("error: cannot remove {}: {e}", dir.join(name).display());
                ok = false;
            }
        }
    }
    ok &= systemctl(&["daemon-reload"]);
    println!("Kept the stored sessions, state and audit log under the meta-mcp home (personal/).");
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| {
            owned
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn socket_unit_listens_in_the_runtime_dir_by_default() {
        let u = render_units(Path::new("/usr/bin/meta-mcp"), None, &[]);
        assert!(
            u.socket
                .contains("ListenStream=%t/meta-mcp/personal.sock\n"),
            "{}",
            u.socket
        );
        for line in [
            "SocketMode=0600",
            "DirectoryMode=0700",
            "RemoveOnStop=yes",
            "WantedBy=sockets.target",
        ] {
            assert!(u.socket.contains(line), "{line}");
        }
        let custom = render_units(Path::new("/x"), Some("/run/user/1/my%sock"), &[]);
        assert!(
            custom
                .socket
                .contains("ListenStream=/run/user/1/my%%sock\n")
        );
    }

    #[test]
    fn service_unit_has_the_hardening_and_the_environment() {
        let pairs = service_env(&env(&[
            ("META_MCP_HOME", "/home/u/.config/meta-mcp"),
            ("META_MCP_FILES_DIR", "/srv/my files"),
            ("META_MCP_READ_ONLY", "1"),
            ("META_MCP_LOG", "meta_mcp=debug"),
        ]))
        .expect("env");
        let u = render_units(Path::new("/opt/meta-mcp"), None, &pairs);
        let s = &u.service;
        assert!(
            s.contains("ExecStart=/opt/meta-mcp personal daemon\n"),
            "{s}"
        );
        assert!(s.contains("Requires=meta-mcp-personal.socket\nAfter=meta-mcp-personal.socket\n"));
        assert!(
            s.contains("Environment=\"META_MCP_HOME=/home/u/.config/meta-mcp\"\n"),
            "{s}"
        );
        assert!(
            s.contains("Environment=\"META_MCP_FILES_DIR=/srv/my files\"\n"),
            "{s}"
        );
        assert!(s.contains("Environment=\"META_MCP_READ_ONLY=1\"\n"));
        assert!(s.contains("Environment=\"META_MCP_LOG=meta_mcp=debug\"\n"));
        assert!(!s.contains("META_MCP_INBOX_DB") && !s.contains("META_MCP_SIDECAR_DIR"));
        for line in [
            "Type=simple",
            "Restart=on-failure",
            "RestartSec=5",
            "RestartPreventExitStatus=2",
            "UMask=0077",
            "NoNewPrivileges=yes",
            "TimeoutStopSec=20",
            "WantedBy=default.target",
        ] {
            assert!(s.contains(&format!("{line}\n")), "{line}");
        }
    }

    #[test]
    fn values_are_quoted_per_systemd_rules() {
        let u = render_units(
            Path::new("/opt/my apps/meta%mcp"),
            None,
            &[("META_MCP_LOG".into(), "a\"b\\c%d".into())],
        );
        assert!(
            u.service
                .contains("ExecStart=\"/opt/my apps/meta%%mcp\" personal daemon\n"),
            "{}",
            u.service
        );
        assert!(
            u.service
                .contains("Environment=\"META_MCP_LOG=a\\\"b\\\\c%%d\"\n"),
            "{}",
            u.service
        );
        assert!(service_env(&env(&[("META_MCP_HOME", "/h"), ("META_MCP_LOG", "a\nb")])).is_err());
    }

    #[test]
    fn the_home_is_resolved_to_an_absolute_path() {
        let pairs = service_env(&env(&[
            ("META_MCP_HOME", "rel/home"),
            ("META_MCP_INBOX_DB", "in.db"),
        ]))
        .expect("env");
        assert!(
            pairs[0].1.starts_with('/') && pairs[0].1.ends_with("rel/home"),
            "{pairs:?}"
        );
        assert!(
            pairs[1].1.starts_with('/') && pairs[1].1.ends_with("in.db"),
            "{pairs:?}"
        );
    }

    #[test]
    fn unit_dir_follows_xdg_then_home() {
        assert_eq!(
            unit_dir(Some("/cfg".into()), Some("/home/u".into())),
            Some(PathBuf::from("/cfg/systemd/user"))
        );
        assert_eq!(
            unit_dir(None, Some("/home/u".into())),
            Some(PathBuf::from("/home/u/.config/systemd/user"))
        );
        assert_eq!(
            unit_dir(Some("relative".into()), Some("/home/u".into())),
            Some(PathBuf::from("/home/u/.config/systemd/user"))
        );
        assert_eq!(unit_dir(None, None), None);
    }
}
