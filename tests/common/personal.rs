//! Personal daemon harness: temp env, fake bash sidecar (both platforms), daemon start and helpers.
//! Include with `#[path = "common/personal.rs"] mod personal_common;` next to `mod common;`.
#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use meta_mcp::inbox::Inbox;
use meta_mcp::personal::client::DaemonClient;
use serde_json::{Value, json};

pub const ACCOUNT_ID: &str = "100001";
pub const COOKIE_SECRET: &str = "xs-cookie-SECRET-value-9f3a";
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Knob files in the fake's root (`<platform>.<knob>`): connect_fail, send_slow, send_unknown, send_fail, send_weird, hello_rotate,
/// list_slow, logout_fail, no_qr (login_start emits no QR), passkey, qr_only (no login after the codes), logged_in (set by login), download_src (download bytes), download_path (file holding the path the sidecar reports).
const FAKE_SIDECAR: &str = r##"#!/bin/bash
PATH=/usr/bin:/bin
ROOT='FAKE_ROOT'
MAIN=$$
PLAT=''
ACCOUNT='{"id":"100001","name":"Test User"}'
echo '{"event":"ready","data":{"pid":'$$',"protocol":1}}'

emit() { echo "$1"; }
ok() { echo '{"id":'$id',"ok":true,"result":'"$1"'}'; }
fail() { echo '{"id":'$id',"ok":false,"error":{"code":"'$1'","message":"'$2'"}}'; }
login_done() {
  touch "$ROOT/$PLAT.logged_in"
  emit '{"event":"state_updated","data":{"state":{"session":"fake-'$PLAT'"}}}'
  emit '{"event":"logged_in","data":{"account":'"$ACCOUNT"'}}'
}
start_emitter() {
  (
    n=0
    while read -r _ _ st _ < /proc/$MAIN/stat 2>/dev/null && [ "$st" != Z ]; do
      total=$(wc -l < "$ROOT/$PLAT.emit" 2>/dev/null || echo 0)
      if [ "$total" -gt "$n" ]; then
        sed -n "$((n+1)),${total}p" "$ROOT/$PLAT.emit" > /proc/$MAIN/fd/1
        n=$total
      fi
      sleep 0.05
    done
  ) >/dev/null &
}

while read -r line; do
  id=${line#*\"id\":}; id=${id%%,*}
  case $line in
    *'"op":"hello"'*)
      PLAT=$(echo "$line" | sed -n 's/.*"platform":"\([a-z]*\)".*/\1/p')
      echo $MAIN > "$ROOT/$PLAT.pid"
      : > "$ROOT/$PLAT.emit"
      start_emitter
      echo "$line" >> "$ROOT/$PLAT.requests.log"
      [ -e "$ROOT/$PLAT.hello_rotate" ] && emit '{"event":"state_updated","data":{"state":{"session":"rotated-'$PLAT'"}}}'
      if [ -e "$ROOT/$PLAT.logged_in" ]; then L=true; A=$ACCOUNT; else L=false; A=null; fi
      ok '{"protocol":1,"sidecar_version":"0.0.0-fake","library":{"name":"fakelib","version":"v0.0.1"},"go":"go1.26.0","logged_in":'$L',"account":'"$A"'}'
      continue;;
  esac
  echo "$line" >> "$ROOT/$PLAT.requests.log"
  case $line in
    *'"op":"ping"'*) ok '{"pong":true}';;
    *'"op":"shutdown"'*) ok '{}'; exit 0;;
    *'"op":"login_start"'*)
      case $line in
        *'"method":"cookies"'*)
          ok '{"needs":["cookies"],"cookie_names":["c_user","xs","datr","sb","fr"],"instructions":"Paste the cookies."}';;
        *'"method":"qr"'*)
          ok '{}'
          if [ -e "$ROOT/$PLAT.passkey" ]; then
            emit '{"event":"passkey_request","data":{"public_key":{"challenge":"abc","rpId":"web.whatsapp.com"},"instructions":"Use a browser tab."}}'
          elif [ -e "$ROOT/$PLAT.no_qr" ]; then
            :
          else
            emit '{"event":"qr","data":{"code":"QR-CODE-ONE","expires_in_s":60}}'
            emit '{"event":"qr","data":{"code":"QR-CODE-TWO","expires_in_s":20}}'
            [ -e "$ROOT/$PLAT.qr_only" ] || login_done
          fi;;
        *'"method":"pair_code"'*)
          ok '{"code":"ABCD-EFGH"}'
          login_done;;
        *) fail bad_params "unknown login method";;
      esac;;
    *'"op":"login_finish"'*)
      case $line in
        *'"cookies"'*) login_done; ok '{"account":'"$ACCOUNT"'}';;
        *'"passkey_response"'*) ok '{}'; emit '{"event":"passkey_confirmation","data":{"code":"123456"}}';;
        *'"confirm":true'*) ok '{}'; login_done;;
        *) fail bad_params "nothing to finish";;
      esac;;
    *'"op":"login_cancel"'*) ok '{}';;
    *'"op":"connect"'*)
      if [ -e "$ROOT/$PLAT.connect_fail" ]; then fail rate_limited "try later"; continue; fi
      ok '{}'
      emit '{"event":"connection","data":{"state":"connected"}}';;
    *'"op":"disconnect"'*) ok '{}';;
    *'"op":"logout"'*)
      if [ -e "$ROOT/$PLAT.logout_fail" ]; then fail io "could not delete the linked device"; continue; fi
      rm -f "$ROOT/$PLAT.logged_in"
      ok '{}'
      emit '{"event":"state_updated","data":{"state":null}}';;
    *'"op":"list_threads"'*)
      [ -e "$ROOT/$PLAT.list_slow" ] && sleep 3
      ok '{"threads":[{"id":"t1","kind":"dm","name":"Alice","participants":[],"last_message_ts":1700000000,"last_message_preview":null,"unread":false,"encrypted":false}],"cursor":null}';;
    *'"op":"fetch_messages"'*)
      ok '{"messages":[{"id":"m1","thread_id":"t1","ts":1700000000,"sender":{"id":"u2","name":"Alice","is_me":false},"text":"hello","attachments":[],"reply_to":null,"reactions":[],"edited":false,"unsent":false,"encrypted":false,"kind":"text"}],"has_more":false}';;
    *'"op":"send_text"'*)
      [ -e "$ROOT/$PLAT.send_slow" ] && sleep 2
      if [ -e "$ROOT/$PLAT.send_weird" ]; then echo '{"id":'$id',"ok":false,"error":{"code":"Weird Code!","message":"nope"}}'
      elif [ -e "$ROOT/$PLAT.send_unknown" ]; then fail send_unknown "outcome uncertain"
      elif [ -e "$ROOT/$PLAT.send_fail" ]; then fail send_failed "the service said no"
      else ok '{"message_id":"mid.1","ts":1700000000}'; fi;;
    *'"op":"send_media"'*)
      path=$(echo "$line" | sed -n 's/.*"path":"\([^"]*\)".*/\1/p')
      cp "$path" "$ROOT/$PLAT.media.copy" 2>/dev/null
      stat -c %a "$path" > "$ROOT/$PLAT.media.mode" 2>/dev/null
      ok '{"message_id":"mid.2","ts":1700000001}';;
    *'"op":"send_reaction"'*) ok '{}';;
    *'"op":"mark_read"'*) ok '{}';;
    *'"op":"set_typing"'*) ok '{}';;
    *'"op":"download_attachment"'*)
      dest=$(echo "$line" | sed -n 's/.*"dest_path":"\([^"]*\)".*/\1/p')
      if [ -e "$ROOT/$PLAT.download_src" ]; then cp "$ROOT/$PLAT.download_src" "$dest"; else printf 'attachment-bytes' > "$dest"; fi
      chmod 600 "$dest"
      sum=$(sha256sum "$dest" | cut -d' ' -f1)
      reported=$dest
      [ -e "$ROOT/$PLAT.download_path" ] && reported=$(cat "$ROOT/$PLAT.download_path")
      ok '{"path":"'"$reported"'","mime":"image/jpeg","bytes":16,"sha256":"'$sum'"}';;
    *) fail bad_params "unknown op";;
  esac
done
"##;

/// Waits (polling) until `cond` holds, panicking with `what` after `timeout`.
pub async fn wait_until<F: FnMut() -> bool>(what: &str, timeout: Duration, mut cond: F) {
    let deadline = Instant::now() + timeout;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Polls `status` until `cond` accepts it, returning the accepted status.
pub async fn wait_status<F: FnMut(&Value) -> bool>(
    client: &DaemonClient,
    what: &str,
    timeout: Duration,
    mut cond: F,
) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let st = client
            .request("status", Value::Null, REQUEST_TIMEOUT)
            .await
            .expect("status");
        if cond(&st) {
            return st;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; last status: {st}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub fn current_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .expect("stat /proc/self")
        .uid()
}

pub struct TestEnv {
    dir: tempfile::TempDir,
}

impl TestEnv {
    /// A fresh environment with both fake sidecars installed and no files folder.
    pub fn new() -> TestEnv {
        let dir = tempfile::Builder::new()
            .prefix("mp")
            .tempdir()
            .expect("temp dir");
        let env = TestEnv { dir };
        for sub in ["fake", "bin", "home"] {
            std::fs::create_dir_all(env.root().join(sub)).expect("mkdir");
        }
        let script = FAKE_SIDECAR.replace("FAKE_ROOT", env.fake_dir().to_str().expect("utf8"));
        for name in ["meta-messenger-sidecar", "meta-whatsapp-sidecar"] {
            let tmp = env.sidecar_dir().join(format!(".{name}.tmp"));
            std::fs::write(&tmp, &script).expect("write script");
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            std::fs::rename(&tmp, env.sidecar_dir().join(name)).expect("install");
        }
        assert!(env.socket().as_os_str().len() < 100, "socket path too long");
        env
    }

    /// Like `new`, with a files folder for media tests.
    pub fn with_files() -> TestEnv {
        let env = TestEnv::new();
        std::fs::create_dir_all(env.files_dir()).expect("files dir");
        env
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn fake_dir(&self) -> PathBuf {
        self.root().join("fake")
    }

    pub fn sidecar_dir(&self) -> PathBuf {
        self.root().join("bin")
    }

    pub fn home(&self) -> PathBuf {
        self.root().join("home")
    }

    pub fn socket(&self) -> PathBuf {
        self.root().join("s.sock")
    }

    pub fn inbox_db(&self) -> PathBuf {
        self.root().join("inbox.db")
    }

    pub fn files_dir(&self) -> PathBuf {
        self.root().join("files")
    }

    pub fn personal_dir(&self) -> PathBuf {
        self.home().join("personal")
    }

    pub fn state_path(&self, platform: &str) -> PathBuf {
        self.personal_dir().join(platform).join("state.json")
    }

    pub fn audit_path(&self) -> PathBuf {
        self.personal_dir().join("audit.jsonl")
    }

    pub fn daemon_log_path(&self) -> PathBuf {
        self.root().join("daemon.err")
    }

    pub fn daemon_log(&self) -> String {
        std::fs::read_to_string(self.daemon_log_path()).unwrap_or_default()
    }

    /// Marks the platform as having a stored session: the fake says logged in and a state file exists.
    pub fn preseed_session(&self, platform: &str) {
        std::fs::write(self.fake_dir().join(format!("{platform}.logged_in")), "").expect("marker");
        let dir = self.personal_dir().join(platform);
        std::fs::create_dir_all(&dir).expect("state dir");
        std::fs::write(
            dir.join("state.json"),
            format!("{{\"session\":\"seeded-{platform}\"}}"),
        )
        .expect("state");
    }

    /// Creates or removes a knob file of the fake (see the list at the top of this file).
    pub fn knob(&self, platform: &str, name: &str, on: bool) {
        let path = self.fake_dir().join(format!("{platform}.{name}"));
        if on {
            std::fs::write(path, "").expect("knob");
        } else {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Request lines the fake sidecar of `platform` received, in order.
    pub fn requests(&self, platform: &str) -> Vec<String> {
        std::fs::read_to_string(self.fake_dir().join(format!("{platform}.requests.log")))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// How many requests of `op` the fake of `platform` received.
    pub fn request_count(&self, platform: &str, op: &str) -> usize {
        let needle = format!("\"op\":\"{op}\"");
        self.requests(platform)
            .iter()
            .filter(|l| l.contains(&needle))
            .count()
    }

    /// The last request line of `op`, parsed.
    pub fn last_request(&self, platform: &str, op: &str) -> Option<Value> {
        let needle = format!("\"op\":\"{op}\"");
        self.requests(platform)
            .iter()
            .rev()
            .find(|l| l.contains(&needle))
            .map(|l| serde_json::from_str(l).expect("request json"))
    }

    /// Makes the fake sidecar of `platform` write this event line to its stdout within ~100 ms.
    pub fn emit(&self, platform: &str, line: &str) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(self.fake_dir().join(format!("{platform}.emit")))
            .expect("emit file");
        writeln!(f, "{line}").expect("emit");
    }

    pub fn sidecar_pid(&self, platform: &str) -> Option<u32> {
        std::fs::read_to_string(self.fake_dir().join(format!("{platform}.pid")))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    /// SIGKILLs the fake sidecar of `platform`.
    pub fn kill_sidecar(&self, platform: &str) {
        let pid = self.sidecar_pid(platform).expect("sidecar pid");
        let status = Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
            .expect("kill");
        assert!(status.success());
    }

    pub fn inbox(&self) -> Inbox {
        Inbox::open(&self.inbox_db()).expect("open inbox")
    }

    /// Every audit line, parsed.
    pub fn audit_lines(&self) -> Vec<Value> {
        std::fs::read_to_string(self.audit_path())
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).expect("audit json"))
            .collect()
    }

    /// Spawns the daemon without waiting for the socket.
    pub fn spawn(&self, extra: &[(&str, &str)]) -> Child {
        self.spawn_on(&self.socket(), extra)
    }

    /// Like `spawn`, listening on `socket` instead of the default one.
    pub fn spawn_on(&self, socket: &Path, extra: &[(&str, &str)]) -> Child {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.daemon_log_path())
            .expect("log");
        // TEST_IGNORE_XFSZ starts the daemon with SIGXFSZ ignored, so a file size limit set later
        // makes writes fail instead of killing it.
        let ignore_xfsz = extra.iter().any(|(k, _)| *k == "TEST_IGNORE_XFSZ");
        let mut cmd = if ignore_xfsz {
            let mut c = Command::new("/bin/sh");
            c.args(["-c", "trap '' XFSZ; exec \"$0\" \"$@\"", crate::common::BIN]);
            c
        } else {
            Command::new(crate::common::BIN)
        };
        cmd.args(["personal", "daemon", "--socket"])
            .arg(socket)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("META_MCP_HOME", self.home())
            .env("META_MCP_INBOX_DB", self.inbox_db())
            .env("META_MCP_SIDECAR_DIR", self.sidecar_dir())
            .env("META_MCP_PERSONAL_BACKOFF_MS", "200,400")
            .env("META_MCP_PERSONAL_SEND_INTERVAL_MS", "1")
            .env("META_MCP_PERSONAL_SEND_BURST", "100000")
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        // The size limit would also hit a log file and make the logger panic, so log nowhere then.
        if ignore_xfsz {
            cmd.stderr(Stdio::null());
        } else {
            cmd.stderr(log);
        }
        if self.files_dir().is_dir() {
            cmd.env("META_MCP_FILES_DIR", self.files_dir());
        }
        for (k, v) in extra.iter().filter(|(k, _)| *k != "TEST_IGNORE_XFSZ") {
            cmd.env(k, v);
        }
        cmd.spawn().expect("spawn the daemon")
    }

    /// Starts the daemon and waits until it accepts connections.
    pub fn start(&self) -> RunningDaemon {
        self.start_with(&[])
    }

    /// Like `start`, with extra environment variables that win over the defaults.
    pub fn start_with(&self, extra: &[(&str, &str)]) -> RunningDaemon {
        let mut child = self.spawn(extra);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().expect("try_wait") {
                panic!("the daemon exited early ({status}):\n{}", self.daemon_log());
            }
            if UnixStream::connect(self.socket()).is_ok() {
                return RunningDaemon { child: Some(child) };
            }
            assert!(
                Instant::now() < deadline,
                "daemon never became ready:\n{}",
                self.daemon_log()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A `meta-mcp <args>` command wired to this environment: the test socket, home, sidecars and
    /// a private XDG_CONFIG_HOME (`<root>/xdg`) for unit files.
    pub fn cli_command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(crate::common::BIN);
        cmd.args(args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("META_MCP_HOME", self.home())
            .env("META_MCP_INBOX_DB", self.inbox_db())
            .env("META_MCP_SIDECAR_DIR", self.sidecar_dir())
            .env("META_MCP_PERSONAL_SOCKET", self.socket())
            .env("XDG_CONFIG_HOME", self.xdg_dir())
            .env("HOME", self.root());
        cmd
    }

    pub fn xdg_dir(&self) -> PathBuf {
        self.root().join("xdg")
    }

    /// Runs `meta-mcp <args>` with `stdin` piped, off the async runtime.
    pub async fn cli(&self, args: &[&str], stdin: &str) -> CliRun {
        self.cli_with(self.cli_command(args), stdin).await
    }

    /// Like `cli`, for a command the test adjusted (extra environment variables).
    pub async fn cli_with(&self, cmd: Command, stdin: &str) -> CliRun {
        let stdin = stdin.to_owned();
        tokio::task::spawn_blocking(move || run_command(cmd, &stdin))
            .await
            .expect("join")
    }

    pub async fn connect(&self) -> DaemonClient {
        DaemonClient::connect(&self.socket())
            .await
            .expect("connect to the daemon socket")
    }

    /// Starts the daemon with a stored session for `platform` and waits until it is connected.
    pub async fn start_logged_in(&self, platform: &str) -> (RunningDaemon, DaemonClient) {
        self.preseed_session(platform);
        let daemon = self.start();
        let client = self.connect().await;
        wait_status(&client, "connected", Duration::from_secs(15), |st| {
            st["platforms"][platform]["connection"] == "connected"
        })
        .await;
        (daemon, client)
    }
}

/// What a `meta-mcp` command printed and how it ended (`code` is -1 when killed by a signal).
#[derive(Debug)]
pub struct CliRun {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Runs `cmd` with `stdin` piped in, killing it after 90 s.
pub fn run_command(mut cmd: Command, stdin: &str) -> CliRun {
    use std::io::{Read, Write};
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the command");
    let mut out = child.stdout.take().expect("stdout");
    let mut err = child.stderr.take().expect("stderr");
    let out_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let mut input = child.stdin.take().expect("stdin");
    let _ = input.write_all(stdin.as_bytes());
    drop(input);
    let code = wait_exit(&mut child).unwrap_or(-1);
    CliRun {
        code,
        stdout: out_thread.join().expect("stdout thread"),
        stderr: err_thread.join().expect("stderr thread"),
    }
}

pub struct RunningDaemon {
    child: Option<Child>,
}

impl RunningDaemon {
    pub fn pid(&self) -> u32 {
        self.child.as_ref().expect("running").id()
    }

    /// SIGTERM, then the exit code (None when killed by a signal).
    pub fn terminate(mut self) -> Option<i32> {
        let mut child = self.child.take().expect("running");
        let status = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .expect("kill");
        assert!(status.success());
        wait_exit(&mut child)
    }

    /// Sets the daemon's file size limit in bytes (`prlimit`), so later writes past it fail.
    pub fn limit_file_size(&self, bytes: u64) {
        let status = Command::new("prlimit")
            .arg(format!("--fsize={bytes}"))
            .arg(format!("--pid={}", self.pid()))
            .status()
            .expect("run prlimit");
        assert!(status.success(), "prlimit failed");
    }

    pub fn is_running(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|c| c.try_wait().expect("try_wait").is_none())
    }
}

impl Drop for RunningDaemon {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

pub fn wait_exit(child: &mut Child) -> Option<i32> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(s) = child.try_wait().expect("try_wait") {
            return s.code();
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("process did not exit within 20s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A `message` event line as a sidecar emits it.
pub fn message_event(id: &str, thread: &str, direction: &str, text: &str) -> String {
    json!({
        "event": "message",
        "data": {
            "direction": direction,
            "message": {
                "id": id, "thread_id": thread, "ts": 1_700_000_100,
                "sender": {"id": "u2", "name": "Alice", "is_me": direction == "out"},
                "text": text,
                "attachments": [{"id": "a1", "kind": "image", "mime": "image/jpeg", "name": "p.jpg"}],
                "reply_to": null, "reactions": [], "edited": false, "unsent": false,
                "encrypted": false, "kind": "text"
            }
        }
    })
    .to_string()
}
