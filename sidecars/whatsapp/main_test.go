package main

import (
	"bufio"
	"bytes"
	"encoding/json"
	"errors"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

func TestLoggerIsScrubbedAndInfoOnly(t *testing.T) {
	var dst bytes.Buffer
	log, scrub := newLogger(&dst)
	log.Info().Str("data", "SECRET").Str("op", "x").Msg("probe")
	log.Debug().Msg("debug must not appear")
	log.Trace().Str("data", "TRACE").Msg("trace must not appear")
	log.Info().Str("jid", "14155550100@s.whatsapp.net").Str("qr", "2@QRSECRET").Str("text", "BODYSECRET").Msg("fields")
	log.Info().Msg("pairing with 14155550100@s.whatsapp.net and 123456789@lid at https://wa.me/settings/linked_devices#QRTOKEN")
	log.Info().Msg("group 120363000000000001@g.us code ABCD-EFGH phone +14155550100")
	_ = scrub.Flush()
	out := dst.String()
	t.Log(strings.TrimSpace(out))
	for _, leak := range []string{"SECRET\"", "TRACE", "must not appear", "QRSECRET", "BODYSECRET", "14155550100", "123456789", "QRTOKEN", "ABCD-EFGH"} {
		if strings.Contains(out, leak) {
			t.Fatalf("output leaks %q: %q", leak, out)
		}
	}
	if !strings.Contains(out, "120363000000000001@g.us") {
		t.Fatalf("group JIDs are not user identifiers and stay readable: %q", out)
	}
	if !strings.Contains(out, `"level":"info"`) || !strings.Contains(out, `"message":"probe"`) || !strings.Contains(out, `"op":"x"`) {
		t.Fatalf("lost fields: %q", out)
	}
}

var (
	buildOnce sync.Once
	binPath   string
	buildErr  error
	buildOut  []byte
)

// sidecarBinary builds the sidecar once per test run with CGO disabled.
func sidecarBinary(t *testing.T) string {
	t.Helper()
	buildOnce.Do(func() {
		dir, err := os.MkdirTemp("", "wa-sidecar-bin-")
		if err != nil {
			buildErr = err
			return
		}
		binPath = filepath.Join(dir, "bin")
		cmd := exec.Command("go", "build", "-o", binPath, ".")
		cmd.Env = append(os.Environ(), "CGO_ENABLED=0", "GOTOOLCHAIN=local")
		buildOut, buildErr = cmd.CombinedOutput()
	})
	if buildErr != nil {
		t.Fatalf("build failed: %v\n%s", buildErr, buildOut)
	}
	return binPath
}

type proc struct {
	cmd   *exec.Cmd
	in    io.WriteCloser
	out   *bufio.Reader
	errCh chan error
}

func spawn(t *testing.T) *proc {
	t.Helper()
	cmd := exec.Command(sidecarBinary(t))
	cmd.Env = []string{}
	in, err := cmd.StdinPipe()
	if err != nil {
		t.Fatal(err)
	}
	out, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	p := &proc{cmd: cmd, in: in, out: bufio.NewReader(out), errCh: make(chan error, 1)}
	t.Cleanup(func() { _ = cmd.Process.Kill() })
	return p
}

func (p *proc) frame(t *testing.T) map[string]any {
	t.Helper()
	type res struct {
		b   []byte
		err error
	}
	ch := make(chan res, 1)
	go func() {
		b, err := p.out.ReadBytes('\n')
		ch <- res{b, err}
	}()
	select {
	case r := <-ch:
		if r.err != nil {
			t.Fatalf("read frame: %v", r.err)
		}
		var m map[string]any
		if err := json.Unmarshal(r.b, &m); err != nil {
			t.Fatalf("bad frame %q: %v", r.b, err)
		}
		return m
	case <-time.After(10 * time.Second):
		t.Fatal("timed out waiting for a frame")
		return nil
	}
}

// exitCode waits for the process and returns its exit status.
func (p *proc) exitCode(t *testing.T) int {
	t.Helper()
	go func() { p.errCh <- p.cmd.Wait() }()
	select {
	case err := <-p.errCh:
		var ee *exec.ExitError
		if errors.As(err, &ee) {
			return ee.ExitCode()
		}
		if err != nil {
			t.Fatalf("wait: %v", err)
		}
		return 0
	case <-time.After(15 * time.Second):
		t.Fatal("process did not exit")
		return -1
	}
}

func wantReady(t *testing.T, p *proc) {
	t.Helper()
	f := p.frame(t)
	data, _ := f["data"].(map[string]any)
	if f["event"] != "ready" || data["protocol"] != float64(1) || data["pid"] == nil {
		t.Fatalf("first frame is not ready: %v", f)
	}
}

func quote(s string) string {
	b, _ := json.Marshal(s)
	return string(b)
}

func TestBinaryHelloPingShutdown(t *testing.T) {
	p := spawn(t)
	wantReady(t, p)
	dir := t.TempDir()
	reqs := []string{
		`{"id":1,"op":"hello","params":{"protocol":1,"platform":"whatsapp","data_dir":` + quote(dir) + `,"state":null}}`,
		`{"id":2,"op":"ping","params":null}`,
		`{"id":3,"op":"shutdown","params":null}`,
	}
	for i, r := range reqs {
		if _, err := io.WriteString(p.in, r+"\n"); err != nil {
			t.Fatal(err)
		}
		f := p.frame(t)
		if f["id"] != float64(i+1) || f["ok"] != true {
			t.Fatalf("request %d: unexpected reply %v", i+1, f)
		}
		if i == 0 {
			res, _ := f["result"].(map[string]any)
			lib, _ := res["library"].(map[string]any)
			if res["logged_in"] != false || res["account"] != nil || res["protocol"] != float64(1) || res["sidecar_version"] != "0.1.0" || lib["name"] != "whatsmeow" {
				t.Fatalf("unexpected hello result %v", res)
			}
		}
	}
	if code := p.exitCode(t); code != 0 {
		t.Fatalf("exit status %d", code)
	}
	for _, name := range []string{"session.db", "messages.db"} {
		fi, err := os.Stat(filepath.Join(dir, name))
		if err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		if fi.Mode().Perm() != 0o600 {
			t.Fatalf("%s mode %v", name, fi.Mode().Perm())
		}
	}
}

func TestBinaryExitsZeroOnStdinClose(t *testing.T) {
	p := spawn(t)
	wantReady(t, p)
	_ = p.in.Close()
	if code := p.exitCode(t); code != 0 {
		t.Fatalf("exit status %d", code)
	}
}

func TestBinaryExitsTwoOnOversizedLine(t *testing.T) {
	p := spawn(t)
	wantReady(t, p)
	go func() {
		line := append(bytes.Repeat([]byte("a"), 17<<20), '\n')
		_, _ = p.in.Write(line)
	}()
	if code := p.exitCode(t); code != 2 {
		t.Fatalf("exit status %d", code)
	}
}

func TestBinaryRefusesSecondProcessOnSameDataDir(t *testing.T) {
	dir := t.TempDir()
	hello := `{"id":1,"op":"hello","params":{"protocol":1,"platform":"whatsapp","data_dir":` + quote(dir) + `,"state":null}}` + "\n"
	first := spawn(t)
	wantReady(t, first)
	if _, err := io.WriteString(first.in, hello); err != nil {
		t.Fatal(err)
	}
	if f := first.frame(t); f["ok"] != true {
		t.Fatalf("first hello %v", f)
	}
	second := spawn(t)
	wantReady(t, second)
	if _, err := io.WriteString(second.in, hello); err != nil {
		t.Fatal(err)
	}
	f := second.frame(t)
	errBody, _ := f["error"].(map[string]any)
	if f["ok"] == true || errBody["code"] != "io" {
		t.Fatalf("second hello %v", f)
	}
	_ = first.in.Close()
	_ = second.in.Close()
	first.exitCode(t)
	second.exitCode(t)
}
