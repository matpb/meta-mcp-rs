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
	"runtime"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/rs/zerolog"
	zlog "github.com/rs/zerolog/log"
)

// TestMain removes the sidecar binary that sidecarBinary built for the run.
func TestMain(m *testing.M) {
	code := m.Run()
	if binPath != "" {
		_ = os.RemoveAll(filepath.Dir(binPath))
	}
	os.Exit(code)
}

func TestLoggerIsScrubbedAndInfoOnly(t *testing.T) {
	var dst bytes.Buffer
	log, scrub := newLogger(&dst)
	log.Info().Str("data", "SECRET").Str("op", "x").Msg("probe")
	log.Debug().Msg("debug must not appear")
	log.Trace().Str("data", "TRACE").Msg("trace must not appear")
	log.Info().Msg("failed with xs=COOKIESECRET; datr=DATRSECRET")
	zlog.Info().Str("token", "GLOBALSECRET").Str("op", "global").Msg("via global xs=GLOBALCOOKIE")
	zlog.Trace().Str("op", "globaltrace").Msg("trace must not appear")
	_ = scrub.Flush()
	if zlog.Logger.GetLevel() != zerolog.InfoLevel {
		t.Fatalf("global logger level is %v", zlog.Logger.GetLevel())
	}
	out := dst.String()
	t.Log(strings.TrimSpace(out))
	for _, leak := range []string{"SECRET\"", "TRACE", "must not appear", "COOKIESECRET", "DATRSECRET", "GLOBALSECRET", "GLOBALCOOKIE", "globaltrace"} {
		if strings.Contains(out, leak) {
			t.Fatalf("output leaks %q: %q", leak, out)
		}
	}
	if !strings.Contains(out, `"op":"global"`) {
		t.Fatalf("global logger output lost: %q", out)
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
		dir, err := os.MkdirTemp("", "sidecar-bin-")
		if err != nil {
			buildErr = err
			return
		}
		binPath = filepath.Join(dir, "bin"+exeSuffix())
		cmd := exec.Command("go", "build", "-o", binPath, ".")
		cmd.Env = append(os.Environ(), "CGO_ENABLED=0")
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

func TestBinaryHelloPingShutdown(t *testing.T) {
	p := spawn(t)
	wantReady(t, p)
	dir := t.TempDir()
	reqs := []string{
		`{"id":1,"op":"hello","params":{"protocol":1,"platform":"messenger","data_dir":` + quote(dir) + `,"state":null}}`,
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
			if res["logged_in"] != false || res["protocol"] != float64(1) || res["sidecar_version"] != "0.1.0" {
				t.Fatalf("unexpected hello result %v", res)
			}
		}
	}
	if code := p.exitCode(t); code != 0 {
		t.Fatalf("exit status %d", code)
	}
}

// M4: two real processes on one data_dir; the second is refused until the first has exited.
func TestBinarySecondProcessOnTheSameDataDirIsRefused(t *testing.T) {
	dir := t.TempDir()
	hello := func(id int) string {
		return `{"id":` + strconv.Itoa(id) + `,"op":"hello","params":{"protocol":1,"platform":"messenger","data_dir":` + quote(dir) + `,"state":null}}` + "\n"
	}
	a, b := spawn(t), spawn(t)
	wantReady(t, a)
	wantReady(t, b)
	if _, err := io.WriteString(a.in, hello(1)); err != nil {
		t.Fatal(err)
	}
	if f := a.frame(t); f["ok"] != true {
		t.Fatalf("first process: %v", f)
	}
	if _, err := io.WriteString(b.in, hello(1)); err != nil {
		t.Fatal(err)
	}
	f := b.frame(t)
	errBody, _ := f["error"].(map[string]any)
	if f["ok"] != false || errBody["code"] != "io" || errBody["message"] != "another sidecar process holds this data_dir" {
		t.Fatalf("second process: %v", f)
	}
	_ = a.in.Close()
	if code := a.exitCode(t); code != 0 {
		t.Fatalf("first process exit status %d", code)
	}
	if _, err := io.WriteString(b.in, hello(2)); err != nil {
		t.Fatal(err)
	}
	if f := b.frame(t); f["ok"] != true {
		t.Fatalf("second process after the first exited: %v", f)
	}
	_ = b.in.Close()
	if code := b.exitCode(t); code != 0 {
		t.Fatalf("second process exit status %d", code)
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

func quote(s string) string {
	b, _ := json.Marshal(s)
	return string(b)
}

func exeSuffix() string {
	if runtime.GOOS == "windows" {
		return ".exe"
	}
	return ""
}
