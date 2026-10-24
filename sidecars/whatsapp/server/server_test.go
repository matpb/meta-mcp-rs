package server

import (
	"context"
	"strings"
	"testing"
	"time"

	"github.com/rs/zerolog"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/wa"
)

func TestReadyEventFirst(t *testing.T) {
	h := startServer(t, &fakeFactory{})
	data := h.event("ready")
	if data["protocol"] != float64(1) || data["pid"] == nil {
		t.Fatalf("ready data %v", data)
	}
}

func TestNotInitialisedBeforeHello(t *testing.T) {
	h := startServer(t, &fakeFactory{})
	for _, op := range []string{"connect", "ping", "list_threads", "send_text"} {
		h.wantErr(h.call(op, nil), api.CodeNotInitialised)
	}
}

func TestHelloErrors(t *testing.T) {
	h := startServer(t, &fakeFactory{})
	good := map[string]any{"protocol": 1, "platform": "whatsapp", "data_dir": h.dir, "state": nil}
	with := func(k string, v any) map[string]any {
		m := map[string]any{}
		for kk, vv := range good {
			m[kk] = vv
		}
		m[k] = v
		return m
	}
	h.wantErr(h.call("hello", nil), api.CodeBadParams)
	h.wantErr(h.call("hello", map[string]any{"protocol": 1}), api.CodeProtocolMismatch)
	h.wantErr(h.call("hello", with("protocol", 2)), api.CodeProtocolMismatch)
	h.wantErr(h.call("hello", with("platform", "messenger")), api.CodeProtocolMismatch)
	h.wantErr(h.call("hello", with("data_dir", "relative/dir")), api.CodeBadParams)
	h.wantErr(h.call("hello", with("data_dir", h.dir+"/missing")), api.CodeBadParams)
	h.wantErr(h.call("hello", with("data_dir", "")), api.CodeBadParams)
	if h.srv.isInit() {
		t.Fatal("failed hellos must not initialise the server")
	}
	h.wantErr(h.call("connect", nil), api.CodeNotInitialised)
}

func TestHelloSuccessUnpaired(t *testing.T) {
	f := &fakeFactory{}
	h := startServer(t, f)
	res := h.mustHello()
	if res["logged_in"] != false || res["account"] != nil || res["protocol"] != float64(1) || res["sidecar_version"] != "0.1.0" {
		t.Fatalf("hello result %v", res)
	}
	lib, _ := res["library"].(map[string]any)
	if lib["name"] != "whatsmeow" || lib["version"] == "" || res["go"] == "" {
		t.Fatalf("library/go %v", res)
	}
	if f.refreshes != 0 {
		t.Fatal("hello must not refresh the WhatsApp version")
	}
	if f.opts[0] != nil {
		t.Fatal("hello builds its client without login options")
	}
	if got := h.pending("state_updated"); len(got) != 0 {
		t.Fatalf("no state expected: %v", got)
	}
}

func TestHelloLoggedInAccount(t *testing.T) {
	h, _ := startPaired(t, false)
	res := h.mustHello()
	if res["logged_in"] != true {
		t.Fatalf("hello result %v", res)
	}
	acc, _ := res["account"].(map[string]any)
	if acc["id"] != selfPN || acc["name"] != "Me" {
		t.Fatalf("account %v", acc)
	}
}

func TestHelloStateWithoutDeviceEmitsNullState(t *testing.T) {
	h := startServer(t, &fakeFactory{})
	rep := h.call("hello", map[string]any{"protocol": 1, "platform": "whatsapp", "data_dir": h.dir, "state": map[string]any{"jid": "x"}})
	h.mustOK(rep)
	ev := h.event("state_updated")
	if v, present := ev["state"]; !present || v != nil {
		t.Fatalf("state_updated %v", ev)
	}
}

func TestHelloTwiceSameAndOtherDir(t *testing.T) {
	f := &fakeFactory{}
	h := startServer(t, f)
	h.mustHello()
	h.mustHello()
	if f.clientCount() != 1 {
		t.Fatalf("second hello must not rebuild the client, clients=%d", f.clientCount())
	}
	h.wantErr(h.call("hello", map[string]any{"protocol": 1, "platform": "whatsapp", "data_dir": t.TempDir(), "state": nil}), api.CodeBadParams)
}

func TestHelloLockedDataDir(t *testing.T) {
	f := &fakeFactory{}
	lock := func(context.Context, string, zerolog.Logger) (wa.Factory, error) { return nil, wa.ErrLocked }
	h := startServerOpen(t, f, lock)
	rep := h.hello()
	h.wantErr(rep, api.CodeIO)
	if !strings.Contains(rep.Error.Message, "another sidecar process") {
		t.Fatalf("message %q", rep.Error.Message)
	}
}

func TestUnknownOpUnsupported(t *testing.T) {
	h := startServer(t, &fakeFactory{})
	h.mustHello()
	h.wantErr(h.call("frobnicate", nil), api.CodeUnsupported)
}

func TestPing(t *testing.T) {
	h := startServer(t, &fakeFactory{})
	h.mustHello()
	if got := h.mustCall("ping", nil); got["pong"] != true {
		t.Fatalf("ping %v", got)
	}
}

func TestShutdownClosesEverything(t *testing.T) {
	f, c := pairedFactory(true)
	h := startServer(t, f)
	h.mustHello()
	h.shutdown()
	if !f.closed {
		t.Fatal("factory not closed")
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.disconns == 0 {
		t.Fatal("client not disconnected")
	}
}

func TestEOFEndsCleanly(t *testing.T) {
	h := startServer(t, &fakeFactory{})
	h.mustHello()
	_ = h.in.Close()
	select {
	case err := <-h.done:
		if err != nil {
			t.Fatalf("EOF returned %v", err)
		}
	case <-time.After(waitFor):
		t.Fatal("Run did not return on EOF")
	}
}

func TestMalformedFrameIsErrFrame(t *testing.T) {
	h := startServer(t, &fakeFactory{})
	if _, err := h.in.Write([]byte("not json\n")); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-h.done:
		if err == nil || !strings.Contains(err.Error(), "bad frame") {
			t.Fatalf("want ErrFrame, got %v", err)
		}
	case <-time.After(waitFor):
		t.Fatal("Run did not stop on a malformed frame")
	}
}

func TestBadParamsAreRejected(t *testing.T) {
	h, _ := startPaired(t, true)
	h.wantErr(h.call("list_threads", map[string]any{"limit": 0}), api.CodeBadParams)
	h.wantErr(h.call("list_threads", map[string]any{"limit": 201}), api.CodeBadParams)
	h.wantErr(h.call("list_threads", map[string]any{"limit": 5, "cursor": "!!!"}), api.CodeBadParams)
	h.wantErr(h.call("list_threads", []int{1}), api.CodeBadParams)
}
