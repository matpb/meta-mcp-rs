package engine

import (
	"context"
	"errors"
	"testing"
	"time"

	"go.mau.fi/mautrix-meta/pkg/messagix"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

func fatalConns(rec *recorder) []api.ConnectionEvent {
	var out []api.ConnectionEvent
	for _, e := range rec.named(api.EvConnection) {
		if c := e.data.(api.ConnectionEvent); c.State == api.ConnFatal {
			out = append(out, c)
		}
	}
	return out
}

// L3: the fatal account signals of the encrypted side end in one connection fatal, stop E2EE and block sending.
func TestE2EEFatalSignalsBlockSending(t *testing.T) {
	cases := []struct {
		name   string
		evt    any
		reason string
	}{
		{"temporary ban", &events.TemporaryBan{Code: events.TempBanSentToTooManyPeople}, "temporary_ban"},
		{"stream replaced", &events.StreamReplaced{}, "stream_replaced"},
		{"client outdated", &events.ClientOutdated{}, "client_outdated"},
		{"logged out", &events.LoggedOut{OnConnect: true, Reason: events.ConnectFailureLoggedOut}, "device_removed"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			s, fc := sendSession(t)
			rec := &recorder{}
			s.sink = rec.sink
			s.setE2EERegistered(true)
			h := s.e2eeHandler(&cycle{})

			if !h(c.evt) {
				t.Fatal("a fatal signal is acked, it must not be redelivered")
			}
			fatal := fatalConns(rec)
			if len(fatal) != 1 || fatal[0].Reason != c.reason || fatal[0].Action != api.ActionNone {
				t.Fatalf("connection events %+v, want one fatal %s", fatal, c.reason)
			}
			if !h(&events.StreamReplaced{}) || !h(&events.TemporaryBan{}) {
				t.Fatal("later signals are acked too")
			}
			if len(fatalConns(rec)) != 1 {
				t.Fatalf("the block is reported once, got %+v", fatalConns(rec))
			}
			if got := s.blockedReason(); got != c.reason {
				t.Fatalf("blocked for %q, want the first signal %q", got, c.reason)
			}

			err := s.Connect(t.Context())
			if ae := wantCode(t, err, api.CodeNotConnected); ae.Msg == "" {
				t.Fatal("connect must say why")
			}
			for _, thread := range []string{"222", "4000", "9000"} {
				_, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: thread, Text: "x"})
				wantCode(t, err, api.CodeNotConnected)
				wantCode(t, s.SendReaction(t.Context(), api.SendReactionParams{ThreadID: thread, MessageID: "fb:mid.$x", Emoji: "x"}), api.CodeNotConnected)
				wantCode(t, s.MarkRead(t.Context(), api.MarkReadParams{ThreadID: thread}), api.CodeNotConnected)
				wantCode(t, s.SetTyping(t.Context(), api.SetTypingParams{ThreadID: thread, On: true}), api.CodeNotConnected)
				_, err = s.SendMedia(t.Context(), api.SendMediaParams{ThreadID: thread, Path: "/nonexistent", Mime: "image/png", Kind: "image"})
				wantCode(t, err, api.CodeNotConnected)
			}
			fc.mu.Lock()
			tasks := len(fc.tasks)
			fc.mu.Unlock()
			if tasks != 0 {
				t.Fatalf("%d tasks reached the socket while blocked", tasks)
			}

			// reads stay available, and the encrypted side is not started again
			if _, err := s.ListThreads(t.Context(), api.ListThreadsParams{Limit: 10}); err != nil {
				t.Fatalf("list_threads while blocked: %v", err)
			}
			s.startE2EE(&cycle{ctx: t.Context(), client: fc})
			for _, e := range rec.named(api.EvError) {
				if e.data.(api.ErrorEvent).Code == codeE2EEUnavailable {
					t.Fatal("E2EE was started again after a fatal signal")
				}
			}
		})
	}
}

func TestE2EELoggedOutMarksTheDeviceUnregistered(t *testing.T) {
	s, _ := sendSession(t)
	rec := &recorder{}
	s.sink = rec.sink
	s.setE2EERegistered(true)
	s.e2eeHandler(&cycle{})(&events.LoggedOut{OnConnect: false})
	if s.State().E2EERegistered {
		t.Fatal("a removed device is no longer registered")
	}
	var sawLoggedOut bool
	for _, e := range rec.named(api.EvError) {
		sawLoggedOut = sawLoggedOut || e.data.(api.ErrorEvent).Code == codeE2EELoggedOut
	}
	if !sawLoggedOut || len(rec.named(api.EvLoggedOut)) != 0 {
		t.Fatalf("want the e2ee_logged_out error and no account logout, got %v", rec.names())
	}
}

// A recovery that was already under way must not touch the device of a blocked account.
func TestRecoverE2EEDoesNothingWhileBlocked(t *testing.T) {
	s, fc := sendSession(t)
	rec := &recorder{}
	s.sink = rec.sink
	s.onE2EEFatal("temporary_ban")
	s.recoverE2EE(&cycle{ctx: t.Context(), client: fc}, events.ConnectFailureNotFound)
	s.mu.Lock()
	recoveries := s.e2eeRecoveries
	s.mu.Unlock()
	if recoveries != 0 {
		t.Fatalf("recoverE2EE ran %d times while blocked", recoveries)
	}
	time.Sleep(10 * time.Millisecond)
}

// After a fatal account signal the daemon holds a fatal connection; later connecting or connected
// events from the still-running Messenger socket must not flip it back.
func TestConnectionEventsStopAfterAFatalSignal(t *testing.T) {
	s, fc := sendSession(t)
	rec := &recorder{}
	s.sink = rec.sink
	cyc := &cycle{ctx: t.Context(), client: fc}
	s.handleEvent(cyc, &messagix.ConnectedEvent{})
	if !hasConn(rec.all(), api.ConnConnected, "") {
		t.Fatalf("before the signal a connect is reported: %v", rec.names())
	}

	s.e2eeHandler(&cycle{})(&events.TemporaryBan{})
	before := len(rec.named(api.EvConnection))
	s.handleEvent(cyc, &messagix.ReconnectedEvent{})
	s.handleEvent(cyc, &messagix.TransientDisconnectEvent{Err: errors.New("net down")})
	s.emitConn(api.ConnConnecting, "", "")
	if got := len(rec.named(api.EvConnection)); got != before {
		t.Fatalf("%d connection events after the fatal signal: %v", got-before, rec.names())
	}
	s.emitConn(api.ConnFatal, "rate_limited", api.ActionWait)
	if got := len(rec.named(api.EvConnection)); got != before+1 {
		t.Fatalf("a further fatal event must still pass, got %d", got-before)
	}
	// reads and messages keep flowing
	s.handleEvent(cyc, &table.LSTable{LSInsertMessage: []*table.LSInsertMessage{
		{MessageId: "mid.$live", ThreadKey: alice, TimestampMs: 9_000_000, SenderId: alice, Text: "still readable"},
	}})
	if len(rec.named(api.EvMessage)) != 1 {
		t.Fatalf("messages must keep arriving while blocked: %v", rec.names())
	}
}

// A promotion or demotion in the group keeps the admin list current, so a demoted admin loses the right to unsend.
func TestGroupInfoKeepsTheAdminListCurrent(t *testing.T) {
	f := newE2EEFixture(t)
	grp := jid("9000", types.GroupServer)
	alice, carol := jid("222", types.MessengerServer), jid("777", types.MessengerServer)
	f.must(t, fbEvent(grp, alice, false, "A1", 1700000000, e2eeTextApp("one")))
	f.must(t, fbEvent(grp, alice, false, "A2", 1700000010, e2eeTextApp("two")))
	f.must(t, fbEvent(grp, alice, false, "A3", 1700000020, e2eeTextApp("three")))
	revoke := func(target, evID string) {
		f.must(t, fbEvent(grp, carol, false, evID, 1700000100, revokeApp(key(target, false, alice.String(), grp.String()))))
	}
	unsent := func(id string) bool { m, _ := f.get(t, "wa:9000@g.us:222@msgr:"+id); return m.Unsent }

	f.must(t, &events.GroupInfo{JID: grp, Promote: []types.JID{carol}})
	revoke("A1", "R1")
	if !unsent("A1") {
		t.Fatal("a member promoted by a group-info event may unsend")
	}
	f.must(t, &events.GroupInfo{JID: grp, Demote: []types.JID{carol}})
	revoke("A2", "R2")
	if unsent("A2") {
		t.Fatal("a demoted admin kept the right to unsend")
	}
	f.must(t, &events.GroupInfo{JID: grp, Promote: []types.JID{carol}})
	f.must(t, &events.GroupInfo{JID: grp, Leave: []types.JID{carol}})
	revoke("A3", "R3")
	if unsent("A3") {
		t.Fatal("an admin who left kept the right to unsend")
	}
	f.must(t, &events.GroupInfo{JID: grp, Name: &types.GroupName{Name: "renamed"}})
	f.must(t, &events.GroupInfo{JID: jid("9999", types.GroupServer), Promote: []types.JID{carol}})
	if e, _ := f.s.threadInfo("9000"); len(e.Admins) != 0 {
		t.Fatalf("admins %v", e.Admins)
	}
}

// A client that connected while its cycle ended, was replaced or got blocked is not left running.
func TestE2EEStaleClientIsDetected(t *testing.T) {
	s, _ := openTest(t, newFakeClient(nil))
	mine, other := new(whatsmeow.Client), new(whatsmeow.Client)
	s.e2mu.Lock()
	s.wa = mine
	s.e2mu.Unlock()
	if s.e2eeStale(t.Context(), mine) {
		t.Fatal("a registered client of a live cycle is not stale")
	}
	if !s.e2eeStale(t.Context(), other) {
		t.Fatal("a client that is no longer the registered one is stale")
	}
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if !s.e2eeStale(ctx, mine) {
		t.Fatal("a client of an ended cycle is stale")
	}
	s.onE2EEFatal("stream_replaced")
	if !s.e2eeStale(t.Context(), mine) {
		t.Fatal("a client that connected after the block is stale")
	}
}
