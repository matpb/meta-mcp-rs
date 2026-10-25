//! Windows: a Task Scheduler logon task for the daemon, plus an immediate windowless start.

pub const TASK_NAME: &str = "meta-mcp personal";
const XML_FILE: &str = "meta-mcp-personal-task.xml";

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

/// Task Scheduler schema 1.2 for a logon task of this user that runs `<exe> personal daemon`
/// without a time limit.
pub fn render_task_xml(exe: &str, user_sid: &str) -> String {
    let exe = xml_escape(exe);
    let sid = xml_escape(user_sid);
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>meta-mcp personal daemon (Messenger and WhatsApp)</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{sid}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{sid}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>3</Count>
    </RestartOnFailure>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <Priority>7</Priority>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
    </IdleSettings>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <Arguments>personal daemon</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

/// UTF-16LE with a byte order mark, the encoding the XML declaration promises.
pub fn encode_utf16le_bom(text: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

/// Drops the `\\?\` prefix `canonicalize` adds when the rest is a plain `X:\...` path.
pub fn strip_verbatim_prefix(path: &str) -> &str {
    match path.strip_prefix(r"\\?\") {
        Some(rest) if rest.starts_with("UNC\\") => path,
        Some(rest) => {
            let b = rest.as_bytes();
            if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
                rest
            } else {
                path
            }
        }
        None => path,
    }
}

/// The note shown when variables are set here: a logon task starts with the saved user environment.
pub fn env_note(set: &[String]) -> Option<String> {
    if set.is_empty() {
        return None;
    }
    Some(format!(
        "note: {} set here. A logon task cannot carry variables and starts with your saved user environment: set them with `setx NAME value` (or the Environment Variables dialog) so the daemon sees them after the next logon. The daemon started now inherits the current values.",
        set.join(", ")
    ))
}

/// The whole `install --dry-run` block (without the sidecar warnings).
pub fn render_install_dry_run(xml: &str, exe: &str, note: Option<&str>) -> String {
    let mut out = format!(
        "# task XML (written to %TEMP%\\{XML_FILE})\n{xml}# commands (dry run: nothing written, nothing run)\nschtasks /Create /TN \"{TASK_NAME}\" /XML %TEMP%\\{XML_FILE} /F\nstart: \"{exe}\" personal daemon (no window)\n"
    );
    if let Some(note) = note {
        out.push_str(note);
        out.push('\n');
    }
    out
}

/// The whole `uninstall --dry-run` block.
pub fn render_uninstall_dry_run() -> String {
    format!(
        "stop: ask the daemon to shut down (op shutdown) and wait up to 20 s\nschtasks /Delete /TN \"{TASK_NAME}\" /F\n(dry run: nothing removed, nothing run; sessions and the audit log are kept)\n"
    )
}

#[cfg(windows)]
pub use run::{install, uninstall};

#[cfg(windows)]
mod run {
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode};
    use std::time::Duration;

    use serde_json::Value;

    use super::{
        TASK_NAME, XML_FILE, encode_utf16le_bom, env_note, render_install_dry_run, render_task_xml,
        render_uninstall_dry_run, strip_verbatim_prefix,
    };
    use crate::personal::cli::install::{
        PASSED_ENV, missing_sidecars, non_empty_env, run_shown, wait_for_daemon,
    };
    use crate::personal::client::DaemonClient;
    use crate::personal::{ENV_SOCKET, ipc};

    const READY_WAIT: Duration = Duration::from_secs(10);
    const STOP_WAIT: Duration = Duration::from_secs(20);

    fn exe_path() -> Result<String, String> {
        let exe = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(|e| format!("cannot resolve the meta-mcp executable path: {e}"))?;
        Ok(strip_verbatim_prefix(&exe.display().to_string()).to_owned())
    }

    fn log_hint() -> String {
        std::env::var_os("LOCALAPPDATA").map_or_else(
            || r"%LOCALAPPDATA%\meta-mcp\logs\daemon.log".to_owned(),
            |base| {
                PathBuf::from(base)
                    .join(r"meta-mcp\logs\daemon.log")
                    .display()
                    .to_string()
            },
        )
    }

    fn env_names_set() -> Vec<String> {
        ["META_MCP_HOME", ENV_SOCKET]
            .into_iter()
            .chain(PASSED_ENV.iter().map(|(name, _)| *name))
            .filter(|name| non_empty_env(name).is_some())
            .map(str::to_owned)
            .collect()
    }

    fn spawn_daemon(exe: &str) -> std::io::Result<()> {
        crate::personal::winsec::spawn_detached(Path::new(exe), "personal daemon").map(drop)
    }

    pub async fn install(dry_run: bool) -> ExitCode {
        let (exe, sid) = match (exe_path(), ipc::own_identity().await) {
            (Ok(exe), Ok(sid)) => (exe, sid),
            (Err(e), _) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
            (_, Err(e)) => {
                eprintln!("error: cannot read the user SID: {e}");
                return ExitCode::FAILURE;
            }
        };
        let xml = render_task_xml(&exe, &sid);
        let note = env_note(&env_names_set());
        let missing = missing_sidecars();
        if dry_run {
            for m in &missing {
                println!("warning: {m}");
            }
            print!("{}", render_install_dry_run(&xml, &exe, note.as_deref()));
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
        let file = std::env::temp_dir().join(XML_FILE);
        if let Err(e) = std::fs::write(&file, encode_utf16le_bom(&xml)) {
            eprintln!("error: cannot write {}: {e}", file.display());
            return ExitCode::FAILURE;
        }
        let file_arg = file.display().to_string();
        let shown = format!("schtasks /Create /TN \"{TASK_NAME}\" /XML {file_arg} /F");
        let mut cmd = Command::new("schtasks");
        cmd.args(["/Create", "/TN", TASK_NAME, "/XML", &file_arg, "/F"]);
        let created = run_shown(cmd, "schtasks", &shown);
        let _ = std::fs::remove_file(&file);
        if !created {
            eprintln!("error: Task Scheduler did not accept the task");
            return ExitCode::FAILURE;
        }
        if let Some(note) = &note {
            println!("{note}");
        }
        if DaemonClient::connect_default().await.is_ok() {
            println!("The daemon is already running.");
        } else if let Err(e) = spawn_daemon(&exe) {
            eprintln!("error: cannot start the daemon: {e}");
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
                log_hint()
            );
            ExitCode::FAILURE
        }
    }

    pub async fn uninstall(dry_run: bool) -> ExitCode {
        if dry_run {
            print!("{}", render_uninstall_dry_run());
            return ExitCode::SUCCESS;
        }
        let mut ok = true;
        if let Ok(client) = DaemonClient::connect_default().await {
            if let Err(e) = client
                .request("shutdown", Value::Null, Duration::from_secs(10))
                .await
            {
                println!("warning: the shutdown request failed: {e}");
            }
            drop(client);
            let deadline = tokio::time::Instant::now() + STOP_WAIT;
            let mut stopped = false;
            while tokio::time::Instant::now() < deadline {
                if DaemonClient::connect_default().await.is_err() {
                    stopped = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            if stopped {
                println!("The daemon stopped.");
            } else {
                eprintln!("error: the daemon still answers 20 s after the shutdown request");
                ok = false;
            }
        }
        let exists = Command::new("schtasks")
            .args(["/Query", "/TN", TASK_NAME])
            .output()
            .is_ok_and(|o| o.status.success());
        if exists {
            let shown = format!("schtasks /Delete /TN \"{TASK_NAME}\" /F");
            let mut cmd = Command::new("schtasks");
            cmd.args(["/Delete", "/TN", TASK_NAME, "/F"]);
            ok &= run_shown(cmd, "schtasks", &shown);
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

    const SID: &str = "S-1-5-21-1-2-3-1001";

    #[test]
    fn the_task_runs_at_this_users_logon_without_a_time_limit() {
        let xml = render_task_xml(r"C:\Tools\meta-mcp.exe", SID);
        for needle in [
            "encoding=\"UTF-16\"",
            "<LogonTrigger>\n      <Enabled>true</Enabled>\n      <UserId>S-1-5-21-1-2-3-1001</UserId>",
            "<Principal id=\"Author\">\n      <UserId>S-1-5-21-1-2-3-1001</UserId>",
            "<LogonType>InteractiveToken</LogonType>",
            "<RunLevel>LeastPrivilege</RunLevel>",
            "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
            "<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>",
            "<RestartOnFailure>\n      <Interval>PT1M</Interval>\n      <Count>3</Count>",
            "<Actions Context=\"Author\">",
            "<Command>C:\\Tools\\meta-mcp.exe</Command>\n      <Arguments>personal daemon</Arguments>",
        ] {
            assert!(xml.contains(needle), "{needle}\n{xml}");
        }
    }

    #[test]
    fn special_characters_are_escaped() {
        let xml = render_task_xml(r#"C:\A&B\<x>'"y".exe"#, "S&1");
        assert!(
            xml.contains("<Command>C:\\A&amp;B\\&lt;x&gt;&apos;&quot;y&quot;.exe</Command>"),
            "{xml}"
        );
        assert!(xml.contains("<UserId>S&amp;1</UserId>"));
    }

    #[test]
    fn utf16_encoding_has_a_bom_and_round_trips() {
        let text = "<a>h\u{e9}llo \u{1F600}</a>";
        let bytes = encode_utf16le_bom(text);
        assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
        let units: Vec<u16> = bytes[2..]
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(String::from_utf16(&units).expect("utf16"), text);
    }

    #[test]
    fn only_a_plain_drive_path_loses_its_verbatim_prefix() {
        assert_eq!(
            strip_verbatim_prefix(r"\\?\C:\Tools\a.exe"),
            r"C:\Tools\a.exe"
        );
        assert_eq!(
            strip_verbatim_prefix(r"\\?\UNC\srv\share\a.exe"),
            r"\\?\UNC\srv\share\a.exe"
        );
        assert_eq!(
            strip_verbatim_prefix(r"\\?\Volume{1}\a.exe"),
            r"\\?\Volume{1}\a.exe"
        );
        assert_eq!(strip_verbatim_prefix(r"C:\a.exe"), r"C:\a.exe");
    }

    #[test]
    fn the_environment_note_names_what_is_set() {
        assert_eq!(env_note(&[]), None);
        let note = env_note(&["META_MCP_HOME".into(), "META_MCP_LOG".into()]).expect("note");
        assert!(note.contains("META_MCP_HOME, META_MCP_LOG"), "{note}");
        assert!(note.contains("setx NAME value"), "{note}");
    }

    #[test]
    fn the_dry_runs_list_exactly_the_planned_steps() {
        assert_eq!(
            render_install_dry_run("<Task/>\n", r"C:\m\meta-mcp.exe", Some("note: x")),
            "# task XML (written to %TEMP%\\meta-mcp-personal-task.xml)\n<Task/>\n# commands (dry run: nothing written, nothing run)\nschtasks /Create /TN \"meta-mcp personal\" /XML %TEMP%\\meta-mcp-personal-task.xml /F\nstart: \"C:\\m\\meta-mcp.exe\" personal daemon (no window)\nnote: x\n"
        );
        assert_eq!(
            render_uninstall_dry_run(),
            "stop: ask the daemon to shut down (op shutdown) and wait up to 20 s\nschtasks /Delete /TN \"meta-mcp personal\" /F\n(dry run: nothing removed, nothing run; sessions and the audit log are kept)\n"
        );
    }
}
