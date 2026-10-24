package engine

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	"go.mau.fi/mautrix-meta/pkg/messagix"
	"go.mau.fi/mautrix-meta/pkg/messagix/dgw"
	"go.mau.fi/mautrix-meta/pkg/messagix/httpclient"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

func hasConn(ev []recordedEvent, state, reason string) bool {
	for _, e := range ev {
		if c, ok := e.data.(api.ConnectionEvent); ok && e.name == api.EvConnection && c.State == state && c.Reason == reason {
			return true
		}
	}
	return false
}

func waitConnect(t *testing.T, fc *fakeClient, rec *recorder) {
	t.Helper()
	rec.wait(t, "client connect", func([]recordedEvent) bool {
		_, connects, _ := fc.counts()
		return connects > 0
	})
}

func TestConnectEventFlow(t *testing.T) {
	fc := newFakeClient(nil)
	fc.page = threadsTable()
	s, rec := openTest(t, fc)
	if _, err := s.Validate(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := s.Connect(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := s.Connect(t.Context()); err != nil {
		t.Fatalf("connect must be idempotent: %v", err)
	}
	waitConnect(t, fc, rec)

	fc.fire(&messagix.ConnectedEvent{})
	rec.wait(t, "initial threads", func(ev []recordedEvent) bool {
		return hasConn(ev, api.ConnConnected, "") && len(rec.named(api.EvThread)) == 3
	})
	if !hasConn(rec.all(), api.ConnConnecting, "") {
		t.Fatalf("missing connecting: %v", rec.names())
	}
	if len(rec.named(api.EvMessage)) != 0 {
		t.Fatal("initial table must not produce message events")
	}
	// The unencrypted side keeps running when the E2EE client cannot start (the fake has no network).
	rec.wait(t, "e2ee_unavailable", func([]recordedEvent) bool {
		for _, e := range rec.named(api.EvError) {
			if e.data.(api.ErrorEvent).Code == "e2ee_unavailable" {
				return true
			}
		}
		return false
	})

	live := &table.LSTable{LSInsertMessage: []*table.LSInsertMessage{
		{MessageId: "mid.$l1", ThreadKey: alice, TimestampMs: 9_000_000, SenderId: alice, Text: "hello"},
	}}
	fc.fire(live)
	rec.wait(t, "live message", func([]recordedEvent) bool { return len(rec.named(api.EvMessage)) == 1 })
	rec.wait(t, "post handle", func([]recordedEvent) bool {
		fc.mu.Lock()
		defer fc.mu.Unlock()
		return len(fc.posted) == 1 && fc.posted[0] == live
	})

	fc.fire(&messagix.TransientDisconnectEvent{Err: errors.New("net down")})
	rec.wait(t, "transient", func(ev []recordedEvent) bool { return hasConn(ev, api.ConnDisconnected, "transient") })
	fc.fire(&messagix.ReconnectedEvent{})
	rec.wait(t, "reconnected", func([]recordedEvent) bool {
		n := 0
		for _, e := range rec.named(api.EvConnection) {
			if e.data.(api.ConnectionEvent).State == api.ConnConnected {
				n++
			}
		}
		return n == 2
	})
	if len(rec.named(api.EvThread)) != 3 {
		t.Fatal("initial table must be processed only once")
	}

	s.Disconnect()
	if !hasConn(rec.all(), api.ConnDisconnected, "requested") {
		t.Fatalf("missing requested disconnect: %v", rec.names())
	}
	if _, _, d := fc.counts(); d == 0 {
		t.Fatal("client must be disconnected")
	}
	s.Disconnect()
	n := 0
	for _, e := range rec.named(api.EvConnection) {
		if e.data.(api.ConnectionEvent).Reason == "requested" {
			n++
		}
	}
	if n != 1 {
		t.Fatalf("second Disconnect must stay quiet, got %d", n)
	}
}

func TestFailClosedOnUnauthorizedSocket(t *testing.T) {
	fc := newFakeClient(nil)
	s, rec := openTest(t, fc)
	if err := s.Connect(t.Context()); err != nil {
		t.Fatal(err)
	}
	waitConnect(t, fc, rec)
	fc.fire(&messagix.PermanentErrorEvent{Err: fmt.Errorf("socket: %w", dgw.ErrUnauthorized)})

	rec.wait(t, "logged_out", func([]recordedEvent) bool { return len(rec.named(api.EvLoggedOut)) == 1 })
	rec.wait(t, "state cleared", func([]recordedEvent) bool { return len(rec.named(api.EvStateUpdated)) == 1 })
	lo := rec.named(api.EvLoggedOut)[0].data.(api.LoggedOutEvent)
	if lo.Reason != "dgw_unauthorized" {
		t.Fatalf("logged_out %+v", lo)
	}
	if st := rec.named(api.EvStateUpdated)[0].data.(api.StateUpdatedEvent); st.State != nil {
		t.Fatalf("state must be null, got %+v", st.State)
	}
	rec.wait(t, "client disconnect", func([]recordedEvent) bool {
		_, _, d := fc.counts()
		return d > 0
	})
	rec.wait(t, "loop ended", func([]recordedEvent) bool {
		s.mu.Lock()
		defer s.mu.Unlock()
		return s.runCancel == nil
	})
	if err := s.Connect(t.Context()); err == nil || err.(*api.Error).Code != api.CodeNotLoggedIn {
		t.Fatalf("connect after logout: %v", err)
	}
	time.Sleep(20 * time.Millisecond)
	if len(rec.named(api.EvLoggedOut)) != 1 {
		t.Fatal("logged_out must be emitted once")
	}
}

func TestFailClosedOnLoadPage(t *testing.T) {
	cases := []struct {
		name       string
		err        error
		reason     string
		clearState bool
	}{
		{"token", httpclient.ErrTokenInvalidated, "token_invalidated", true},
		{"checkpoint", httpclient.RedirectedError{Type: httpclient.ErrCheckpointRequired, URL: "https://x/checkpoint/"}, "checkpoint_required", false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			fc := newFakeClient(nil)
			fc.loadErr = tc.err
			s, rec := openTest(t, fc)
			if err := s.Connect(t.Context()); err != nil {
				t.Fatal(err)
			}
			rec.wait(t, "logged_out", func([]recordedEvent) bool { return len(rec.named(api.EvLoggedOut)) == 1 })
			lo := rec.named(api.EvLoggedOut)[0].data.(api.LoggedOutEvent)
			if lo.Reason != tc.reason || (tc.reason == "checkpoint_required" && lo.URL == "") {
				t.Fatalf("logged_out %+v", lo)
			}
			time.Sleep(20 * time.Millisecond)
			if got := len(rec.named(api.EvStateUpdated)) == 1; got != tc.clearState {
				t.Fatalf("state cleared = %v, want %v", got, tc.clearState)
			}
		})
	}
}

func TestFatalConnectionStopsLoop(t *testing.T) {
	fc := newFakeClient(nil)
	fc.loadErr = httpclient.ErrRateLimited
	s, rec := openTest(t, fc)
	if err := s.Connect(t.Context()); err != nil {
		t.Fatal(err)
	}
	rec.wait(t, "fatal", func(ev []recordedEvent) bool {
		for _, e := range ev {
			if c, ok := e.data.(api.ConnectionEvent); ok && c.State == api.ConnFatal && c.Reason == "rate_limited" && c.Action == api.ActionWait {
				return true
			}
		}
		return false
	})
	rec.wait(t, "loop ended", func([]recordedEvent) bool {
		s.mu.Lock()
		defer s.mu.Unlock()
		return s.runCancel == nil
	})
	if len(rec.named(api.EvLoggedOut)) != 0 {
		t.Fatal("rate limiting must not log out")
	}
	fc.mu.Lock()
	fc.loadErr = nil
	fc.mu.Unlock()
	if err := s.Connect(t.Context()); err != nil {
		t.Fatalf("connect must be callable again: %v", err)
	}
	waitConnect(t, fc, rec)
}

func TestTransientLoadRetries(t *testing.T) {
	fc := newFakeClient(nil)
	fc.loadErr = errors.New("dial tcp: refused")
	s, rec := openTest(t, fc)
	if err := s.Connect(t.Context()); err != nil {
		t.Fatal(err)
	}
	rec.wait(t, "transient", func(ev []recordedEvent) bool { return hasConn(ev, api.ConnDisconnected, "transient") })
	fc.mu.Lock()
	fc.loadErr = nil
	fc.mu.Unlock()
	waitConnect(t, fc, rec)
}

func TestPermanentErrorOtherIsFatalConn(t *testing.T) {
	fc := newFakeClient(nil)
	s, rec := openTest(t, fc)
	if err := s.Connect(t.Context()); err != nil {
		t.Fatal(err)
	}
	waitConnect(t, fc, rec)
	fc.fire(&messagix.PermanentErrorEvent{Err: httpclient.ErrVersionIDNotFound})
	rec.wait(t, "fatal", func(ev []recordedEvent) bool {
		for _, e := range ev {
			if c, ok := e.data.(api.ConnectionEvent); ok && c.State == api.ConnFatal && c.Reason == "permanent_error" && c.Action == api.ActionNone {
				return true
			}
		}
		return false
	})
}

func TestLogoutWipesLocalData(t *testing.T) {
	fc := newFakeClient(nil)
	s, _ := openTest(t, fc)
	s.processTable(threadsTable(), modeInitial)
	if _, err := s.ensureStore(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := s.Logout(t.Context()); err != nil {
		t.Fatal(err)
	}
	st := s.State()
	if len(st.Cookies) != 0 || st.FBID != "111" || st.E2EERegistered {
		t.Fatalf("state after logout %+v", st)
	}
	for _, name := range []string{"e2ee.db", threadsFileName} {
		if _, err := os.Stat(filepath.Join(s.dataDir, name)); err == nil {
			t.Fatalf("%s must be removed", name)
		}
	}
	s.Close()
	s.Close()
	if err := s.Connect(t.Context()); err == nil {
		t.Fatal("connect after logout must fail")
	}
}
