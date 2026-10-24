package server

import (
	"errors"
	"strings"
	"testing"
	"time"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
)

func TestConnectedSendsUnavailablePresenceAndEmitsConnected(t *testing.T) {
	h, c := startPaired(t, true)
	if !c.deliver(&events.Connected{}) {
		t.Fatal("refused")
	}
	select {
	case p := <-c.presences:
		if p != types.PresenceUnavailable {
			t.Fatalf("presence %v", p)
		}
	case <-time.After(waitFor):
		t.Fatal("no presence sent")
	}
	if ev := h.event("connection"); ev["state"] != "connected" {
		t.Fatalf("connection %v", ev)
	}
}

func TestConnectNeedsDevice(t *testing.T) {
	f := &fakeFactory{}
	h := startServer(t, f)
	h.mustHello()
	h.wantErr(h.call("connect", nil), api.CodeNotLoggedIn)
	if f.refreshes != 0 {
		t.Fatal("no version refresh without a device")
	}
}

func TestConnectRefreshesVersionAndConnects(t *testing.T) {
	h, c := startPaired(t, false)
	h.mustCall("connect", nil)
	if ev := h.event("connection"); ev["state"] != "connecting" {
		t.Fatalf("connection %v", ev)
	}
	c.mu.Lock()
	connects := c.connects
	c.mu.Unlock()
	if connects != 1 || h.f.refreshes != 1 {
		t.Fatalf("connects %d refreshes %d", connects, h.f.refreshes)
	}
	h.mustCall("connect", nil)
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.connects != 1 {
		t.Fatal("connect on a connected client must be a no-op")
	}
}

func TestConnectFailureReportsDisconnected(t *testing.T) {
	h, c := startPaired(t, false)
	c.connectErr = errors.New("dial tcp 1.2.3.4: refused")
	rep := h.call("connect", nil)
	h.wantErr(rep, api.CodeIO)
	if rep.Error.Message == "" || rep.Error.Message == c.connectErr.Error() {
		t.Fatalf("error message must be fixed text, got %q", rep.Error.Message)
	}
	h.event("connection")
	ev := h.event("connection")
	if ev["state"] != "disconnected" || ev["reason"] != "connect_error" || ev["action"] != "wait" {
		t.Fatalf("connection %v", ev)
	}
}

func TestDisconnectOp(t *testing.T) {
	h, c := startPaired(t, true)
	h.mustCall("disconnect", nil)
	ev := h.event("connection")
	if ev["state"] != "disconnected" || ev["reason"] != "requested" || ev["action"] != "none" {
		t.Fatalf("connection %v", ev)
	}
	if c.IsConnected() {
		t.Fatal("client still connected")
	}
}

func TestDropsAreReportedOnlyWhenConnectionWasRequested(t *testing.T) {
	h, c := startPaired(t, false)
	c.deliver(&events.Disconnected{})
	if got := h.pending("connection"); len(got) != 0 {
		t.Fatalf("unrequested drop reported: %v", got)
	}
	h.mustCall("connect", nil)
	h.consume("connection")
	c.deliver(&events.Disconnected{})
	if ev := h.event("connection"); ev["reason"] != "network" || ev["action"] != "wait" {
		t.Fatalf("connection %v", ev)
	}
	c.deliver(&events.KeepAliveTimeout{})
	if ev := h.event("connection"); ev["reason"] != "keepalive_timeout" {
		t.Fatalf("connection %v", ev)
	}
	c.deliver(&events.KeepAliveRestored{})
	if ev := h.event("connection"); ev["state"] != "connected" {
		t.Fatalf("connection %v", ev)
	}
	c.deliver(&events.ConnectFailure{Reason: events.ConnectFailureServiceUnavailable})
	if ev := h.event("connection"); ev["reason"] != "connect_failure_503" || ev["action"] != "retry" {
		t.Fatalf("connection %v", ev)
	}
	if got := h.pending("connection"); len(got) != 0 {
		t.Fatalf("non-fatal failures must not block sends: %v", got)
	}
}

func TestFatalEventsBlockSending(t *testing.T) {
	cases := []struct {
		name   string
		evt    any
		reason string
		action string
	}{
		{"temporary ban", &events.TemporaryBan{Code: events.TempBanSentToTooManyPeople}, "temporary_ban", "wait"},
		{"reachout timelock", &events.NotifyAccountReachoutTimelock{IsActive: true}, "reachout_timelock", "wait"},
		{"stream replaced", &events.StreamReplaced{}, "stream_replaced", "none"},
		{"client outdated", &events.ClientOutdated{}, "client_outdated", "none"},
		{"logged out 401", &events.LoggedOut{OnConnect: true, Reason: events.ConnectFailureLoggedOut}, "device_removed", "relogin"},
		{"logged out stream error", &events.LoggedOut{}, "device_removed", "relogin"},
		{"logged out 406", &events.LoggedOut{OnConnect: true, Reason: events.ConnectFailureUnknownLogout}, "banned", "relogin"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			h, c := startPaired(t, true)
			h.incoming(c, peerPN, "A1", 1700000100, "hi")
			h.consume("thread")
			h.consume("message")
			if !c.deliver(tc.evt) {
				t.Fatal("fatal event refused")
			}
			ev := h.event("connection")
			if ev["state"] != "fatal" || ev["reason"] != tc.reason || ev["action"] != tc.action {
				t.Fatalf("connection %v", ev)
			}
			rep := h.call("send_text", map[string]any{"thread_id": peerPN, "text": "x"})
			h.wantErr(rep, api.CodeSendingBlocked)
			if !strings.Contains(rep.Error.Message, tc.reason) {
				t.Fatalf("the error must name the reason: %q", rep.Error.Message)
			}
			for op, params := range map[string]any{
				"send_media":    map[string]any{"thread_id": peerPN, "path": "/x", "mime": "image/png", "kind": "image"},
				"send_reaction": map[string]any{"thread_id": peerPN, "message_id": "A1", "emoji": "x"},
				"mark_read":     map[string]any{"thread_id": peerPN},
				"set_typing":    map[string]any{"thread_id": peerPN, "on": true},
			} {
				h.wantErr(h.call(op, params), api.CodeSendingBlocked)
			}
			if tc.reason == "device_removed" || tc.reason == "banned" {
				lo := h.event("logged_out")
				st := h.event("state_updated")
				if lo["reason"] != tc.reason {
					t.Fatalf("logged_out %v", lo)
				}
				if v, present := st["state"]; !present || v != nil {
					t.Fatalf("state_updated %v", st)
				}
				eventually(t, "fresh client", func() bool { return h.f.clientCount() == 2 })
				if res := h.mustHello(); res["logged_in"] != false {
					t.Fatalf("hello after logout %v", res)
				}
			}
		})
	}
}

func TestInactiveReachoutTimelockIsIgnored(t *testing.T) {
	h, c := startPaired(t, true)
	if !c.deliver(&events.NotifyAccountReachoutTimelock{IsActive: false}) {
		t.Fatal("refused")
	}
	if got := h.pending("connection"); len(got) != 0 {
		t.Fatalf("connection %v", got)
	}
}

func TestRelinkKeepsBlockUntilRestart(t *testing.T) {
	h, c := startPaired(t, true)
	c.deliver(&events.LoggedOut{OnConnect: true, Reason: events.ConnectFailureLoggedOut})
	eventually(t, "fresh client", func() bool { return h.f.clientCount() == 2 })
	if h.srv.blockedReason() != "device_removed" {
		t.Fatalf("blocked %q", h.srv.blockedReason())
	}
	h.mustCall("login_start", map[string]any{"method": "qr"})
	c3 := h.f.client(2)
	c3.pair()
	c3.deliver(&events.Connected{})
	c3.qr <- whatsmeow.QRChannelSuccess
	h.event("logged_in")
	if h.srv.blockedReason() != "device_removed" {
		t.Fatalf("sends stay blocked until restart (Amendment 1), got %q", h.srv.blockedReason())
	}
}

func TestStaleClientEventsAreIgnored(t *testing.T) {
	h, c := startPaired(t, true)
	h.incoming(c, peerPN, "A1", 1700000100, "hi")
	h.mustCall("logout", nil)
	h.consume("thread")
	h.consume("message")
	h.consume("state_updated")
	if !c.deliver(&events.StreamReplaced{}) {
		t.Fatal("stale events must be acked")
	}
	if got := h.pending("connection"); len(got) != 0 {
		t.Fatalf("stale client event leaked: %v", got)
	}
	if h.srv.blockedReason() != "" {
		t.Fatal("stale fatal event blocked sending")
	}
}
