package server

import (
	"bufio"
	"context"
	"fmt"
	"io"
	"strings"
	"testing"
	"time"

	"github.com/rs/zerolog"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waCommon"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"

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
	for _, op := range []string{"connect", "list_threads", "send_text", "logout", "fetch_messages", "frobnicate"} {
		h.wantErr(h.call(op, nil), api.CodeNotInitialised)
	}
}

func TestPingIsAnsweredBeforeHello(t *testing.T) {
	h := startServer(t, &fakeFactory{})
	if got := h.mustCall("ping", nil); got["pong"] != true {
		t.Fatalf("ping %v", got)
	}
	if h.srv.isInit() {
		t.Fatal("ping must not initialise the server")
	}
	h.wantErr(h.call("connect", nil), api.CodeNotInitialised)
	h.mustHello()
	if got := h.mustCall("ping", nil); got["pong"] != true {
		t.Fatalf("ping after hello %v", got)
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
	f, _ := pairedFactory(false)
	h := startServer(t, f)
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

func TestSecondHelloIsRefused(t *testing.T) {
	f := &fakeFactory{}
	h := startServer(t, f)
	h.mustHello()
	for _, dir := range []string{h.dir, t.TempDir()} {
		rep := h.call("hello", map[string]any{"protocol": 1, "platform": "whatsapp", "data_dir": dir, "state": nil})
		h.wantErr(rep, api.CodeBadParams)
		if rep.Error.Message != "already initialised" {
			t.Fatalf("message %q", rep.Error.Message)
		}
	}
	h.wantErr(h.call("hello", nil), api.CodeBadParams)
	if f.clientCount() != 1 {
		t.Fatalf("a second hello must not rebuild the client, clients=%d", f.clientCount())
	}
	if got := h.mustCall("ping", nil); got["pong"] != true {
		t.Fatalf("the server must keep working: %v", got)
	}
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
	rep := h.call("frobnicate", nil)
	h.wantErr(rep, api.CodeUnsupported)
	if rep.Error.Message != "no such op" {
		t.Fatalf("message %q", rep.Error.Message)
	}
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

func TestPanicInAnOpIsAnInternalReply(t *testing.T) {
	f, c := pairedFactory(true)
	h, logs := startServerLogged(t, f)
	h.mustHello()
	h.incoming(c, peerPN, "A1", 1700000100, "hi")
	h.consume("thread")
	h.consume("message")
	c.sendFn = func(types.JID, *waE2E.Message, whatsmeow.SendRequestExtra) (whatsmeow.SendResponse, error) {
		panic("boom carrying 14155550100 and a message body")
	}
	rep := h.call("send_text", map[string]any{"thread_id": peerPN, "text": "x"})
	h.wantErr(rep, api.CodeInternal)
	if rep.Error.Message != "internal error" {
		t.Fatalf("the reply must not carry the panic value: %q", rep.Error.Message)
	}
	ev := h.event("error")
	if ev["code"] != api.CodeInternal || strings.Contains(fmt.Sprint(ev), "boom") {
		t.Fatalf("error event %v", ev)
	}
	out := logs.String()
	if strings.Contains(out, "boom") || strings.Contains(out, "14155550100") || !strings.Contains(out, `"component":"op"`) || !strings.Contains(out, `"op":"send_text"`) || !strings.Contains(out, `"reason":"string"`) {
		t.Fatalf("the log names the panic by type only: %s", out)
	}
	c.sendFn = nil
	h.mustCall("send_text", map[string]any{"thread_id": peerPN, "text": "still alive"})
}

func TestPanicInAnEventHandlerIsNotAcknowledged(t *testing.T) {
	f, c := pairedFactory(false)
	h, logs := startServerLogged(t, f)
	h.mustHello()
	c.decReaction = func(*events.Message) (*waE2E.ReactionMessage, error) { panic("boom with secret text") }
	enc := &waE2E.Message{EncReactionMessage: &waE2E.EncReactionMessage{TargetMessageKey: &waCommon.MessageKey{ID: ptr("A1")}}}
	h.incoming(c, peerPN, "A1", 1700000100, "hi")
	if c.deliver(msgEvent(peerPN, peerPN, false, "R1", 1700000160, enc)) {
		t.Fatal("a panicking handler must not acknowledge the event")
	}
	ev := h.event("error")
	if ev["code"] != api.CodeInternal || strings.Contains(fmt.Sprint(ev), "secret") {
		t.Fatalf("error event %v", ev)
	}
	if out := logs.String(); strings.Contains(out, "secret") || !strings.Contains(out, `"component":"event"`) || !strings.Contains(out, `"event":"*events.Message"`) {
		t.Fatalf("log %s", out)
	}
	c.decReaction = nil
	h.incoming(c, peerPN, "A2", 1700000200, "the next event still works")
}

func TestPanicInABackgroundGoroutineIsRecovered(t *testing.T) {
	f, c := pairedFactory(false)
	h, logs := startServerLogged(t, f, withTiming(func(*timing) {}))
	h.mustHello()
	c.groupPanic = true
	c.deliver(groupMsg(malloryPN, "G1", 1700000100, text("hello group")))
	<-c.groupCall
	ev := h.event("error")
	if ev["code"] != api.CodeInternal {
		t.Fatalf("error event %v", ev)
	}
	eventually(t, "recovered panic logged", func() bool { return strings.Contains(logs.String(), `"component":"background/group"`) })
	if got := h.mustCall("ping", nil); got["pong"] != true {
		t.Fatalf("the process must keep serving: %v", got)
	}
	eventually(t, "group fetch released", func() bool {
		h.srv.groups.mu.Lock()
		defer h.srv.groups.mu.Unlock()
		return !h.srv.groups.inflight
	})
}

func TestPanicInHelloIsAnInternalReply(t *testing.T) {
	panicking := func(context.Context, string, zerolog.Logger) (wa.Factory, error) { panic("open exploded") }
	h := startServerOpen(t, &fakeFactory{}, panicking)
	rep := h.hello()
	h.wantErr(rep, api.CodeInternal)
	if h.srv.isInit() {
		t.Fatal("a panicking hello must not initialise the server")
	}
	if got := h.mustCall("ping", nil); got["pong"] != true {
		t.Fatalf("ping %v", got)
	}
}

// Review: a message is acknowledged only after its store commit.
func TestReviewNoAckWhenStoreFails(t *testing.T) {
	h, c := startPaired(t, false)
	h.incoming(c, peerPN, "A0", 1700000050, "hi")
	_ = h.srv.store.Close()
	if c.deliver(msgEvent(peerPN, peerPN, false, "A1", 1700000060, text("second"))) {
		t.Errorf("handler acked a message although the store is closed")
	}
}

// Review: a message is acknowledged only after its event reached stdout.
func TestReviewNoAckWhenStdoutFails(t *testing.T) {
	f, c := pairedFactory(false)
	inR, inW := io.Pipe()
	outR, outW := io.Pipe()
	ready := make(chan struct{})
	go func() {
		_ = run(context.Background(), inR, outW, zerolog.Nop(), func(context.Context, string, zerolog.Logger) (wa.Factory, error) { return f, nil }, func(*server) { close(ready) })
		_ = outW.Close()
	}()
	<-ready
	br := bufio.NewReader(outR)
	_, _ = br.ReadBytes('\n') // ready
	_, _ = io.WriteString(inW, fmt.Sprintf(`{"id":1,"op":"hello","params":{"protocol":1,"platform":"whatsapp","data_dir":%q,"state":null}}`+"\n", t.TempDir()))
	_, _ = br.ReadBytes('\n') // hello reply
	_ = outR.Close()          // every later write fails
	if c.deliver(msgEvent(peerPN, peerPN, false, "A2", 1700000060, text("third"))) {
		t.Errorf("handler acked a message although stdout is broken")
	}
	_ = inW.Close()
}
