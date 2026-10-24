package engine

import (
	"context"
	"errors"
	"testing"
	"time"

	"go.mau.fi/mautrix-meta/pkg/messagix"
	"go.mau.fi/mautrix-meta/pkg/messagix/httpclient"
	"go.mau.fi/mautrix-meta/pkg/messagix/socket"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
	"go.mau.fi/whatsmeow/types/events"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

func fastBackoff(t *testing.T) {
	t.Helper()
	old := backoffMin
	backoffMin = 100 * time.Microsecond
	t.Cleanup(func() { backoffMin = old })
}

func loadsOf(fc *fakeClient) int {
	l, _, _ := fc.counts()
	return l
}

func hasRepeatedFailures(rec *recorder) int {
	n := 0
	for _, c := range fatalConns(rec) {
		if c.Reason == "repeated_failures" && c.Action == api.ActionRelogin {
			n++
		}
	}
	return n
}

// L3: six page loads in a row that fail without a verdict end the retry loop with a fatal connection event.
func TestRepeatedPageFailuresStopTheLoop(t *testing.T) {
	fastBackoff(t)
	fc := newFakeClient(nil)
	fc.loadSeq = func(int) error { return errors.New("boom") }
	s, rec := openTest(t, fc)
	if err := s.Connect(t.Context()); err != nil {
		t.Fatal(err)
	}
	rec.wait(t, "repeated_failures", func([]recordedEvent) bool { return hasRepeatedFailures(rec) == 1 })
	time.Sleep(60 * time.Millisecond)
	if got := loadsOf(fc); got != maxPageFailures {
		t.Fatalf("%d page loads, want exactly %d and then silence", got, maxPageFailures)
	}
	if hasRepeatedFailures(rec) != 1 {
		t.Fatal("the fatal is reported once")
	}
	if len(rec.named(api.EvLoggedOut)) != 0 {
		t.Fatal("repeated failures are not a logout")
	}

	// the daemon's own reconnect loop calls connect again: a fresh budget of failures
	if err := s.Connect(t.Context()); err != nil {
		t.Fatal(err)
	}
	rec.wait(t, "second repeated_failures", func([]recordedEvent) bool { return hasRepeatedFailures(rec) == 2 })
	if got := loadsOf(fc); got != 2*maxPageFailures {
		t.Fatalf("%d page loads after the second connect, want %d", got, 2*maxPageFailures)
	}
}

func TestAPageThatLoadsResetsTheFailureCount(t *testing.T) {
	fastBackoff(t)
	fc := newFakeClient(nil)
	fc.loadSeq = func(n int) error {
		if n < 6 || (n > 6 && n < 12) {
			return errors.New("boom")
		}
		return nil
	}
	s, rec := openTest(t, fc)
	if err := s.Connect(t.Context()); err != nil {
		t.Fatal(err)
	}
	waitConnect(t, fc, rec)
	if got := loadsOf(fc); got != 6 {
		t.Fatalf("%d loads before the first success", got)
	}
	s.requestRecycle(true)
	deadline := time.Now().Add(5 * time.Second)
	for loadsOf(fc) < 12 && time.Now().Before(deadline) {
		time.Sleep(5 * time.Millisecond)
	}
	time.Sleep(30 * time.Millisecond)
	if got := loadsOf(fc); got != 12 {
		t.Fatalf("%d loads, want 12 (5 failures, then a success)", got)
	}
	if hasRepeatedFailures(rec) != 0 {
		t.Fatal("two runs of five failures split by a success must not trip the cap")
	}
}

func TestClassifiedFailureDoesNotCountTowardsTheCap(t *testing.T) {
	fastBackoff(t)
	fc := newFakeClient(nil)
	fc.loadSeq = func(n int) error {
		if n < 5 {
			return errors.New("boom")
		}
		return httpclient.ErrTokenInvalidated
	}
	s, rec := openTest(t, fc)
	if err := s.Connect(t.Context()); err != nil {
		t.Fatal(err)
	}
	rec.wait(t, "logged_out", func([]recordedEvent) bool { return len(rec.named(api.EvLoggedOut)) == 1 })
	if hasRepeatedFailures(rec) != 0 {
		t.Fatal("a classified failure is its own verdict")
	}
}

// L7: a panic inside a connect cycle is contained, reported and retried.
func TestConnectCyclePanicIsRetried(t *testing.T) {
	fastBackoff(t)
	fc := newFakeClient(nil)
	fc.loadSeq = func(n int) error {
		if n == 1 {
			panic("SECRET message text in a panic value")
		}
		return nil
	}
	s, rec := openTest(t, fc)
	if err := s.Connect(t.Context()); err != nil {
		t.Fatal(err)
	}
	waitConnect(t, fc, rec)
	if got := loadsOf(fc); got != 2 {
		t.Fatalf("%d loads, want the panicked one and the retry", got)
	}
	errs := rec.named(api.EvError)
	if len(errs) != 1 {
		t.Fatalf("want one error event, got %v", rec.names())
	}
	if ev := errs[0].data.(api.ErrorEvent); ev.Code != api.CodeInternal || ev.Message == "" {
		t.Fatalf("error event %+v", ev)
	}
}

// L7: a panic in the Lightspeed event consumer is reported, and the next event is still handled.
func TestEventHandlerPanicIsContainedAndReported(t *testing.T) {
	fc := newFakeClient(nil)
	s, rec := openTest(t, fc)
	cyc := &cycle{ctx: t.Context(), client: fc}
	s.safeHandle(nil, &messagix.ConnectedEvent{})
	if errs := rec.named(api.EvError); len(errs) != 1 || errs[0].data.(api.ErrorEvent).Code != api.CodeInternal {
		t.Fatalf("want one internal error event, got %v", rec.names())
	}
	s.safeHandle(cyc, &table.LSTable{LSInsertMessage: []*table.LSInsertMessage{
		{MessageId: "mid.$after", ThreadKey: alice, TimestampMs: 9_000_000, SenderId: alice, Text: "still alive"},
	}})
	if len(rec.named(api.EvMessage)) != 1 {
		t.Fatalf("the consumer stopped after a panic: %v", rec.names())
	}
}

// L7: a panic in the encrypted client setup is contained and reported.
func TestStartE2EEPanicIsContained(t *testing.T) {
	fc := newFakeClient(nil)
	fc.setDevice = func() { panic("SECRET from whatsmeow") }
	s, rec := openTest(t, fc)
	s.startE2EE(&cycle{ctx: t.Context(), client: fc})
	errs := rec.named(api.EvError)
	if len(errs) != 1 || errs[0].data.(api.ErrorEvent).Code != api.CodeInternal {
		t.Fatalf("want one internal error event, got %v", rec.names())
	}
}

// L7: a panic in the E2EE handler is not acked and is reported without its value.
func TestE2EEHandlerPanicEmitsAnErrorEvent(t *testing.T) {
	fc := newFakeClient(nil)
	s, rec := openTest(t, fc)
	if s.e2eeHandler(&cycle{})((*events.ConnectFailure)(nil)) {
		t.Fatal("a panicking handler must not ack")
	}
	errs := rec.named(api.EvError)
	if len(errs) != 1 {
		t.Fatalf("want one error event, got %v", rec.names())
	}
	if ev := errs[0].data.(api.ErrorEvent); ev.Code != api.CodeInternal || ev.Message != "an internal error was contained" {
		t.Fatalf("error event %+v", ev)
	}
}

// L4: every page of a long history hands over a message, and the call still ends at the cap with what it has.
func TestFetchMessagesStopsAtThePageCapWithWhatItHas(t *testing.T) {
	fc := newFakeClient(nil)
	calls := 0
	cur := int64(100_000)
	fc.exec = func(_ context.Context, tasks ...socket.Task) (*table.LSTable, error) {
		calls++
		cur -= 1000
		return upsertTable(histThread, true, cur), nil
	}
	s, _ := openTest(t, fc)
	if _, err := s.Validate(t.Context()); err != nil {
		t.Fatal(err)
	}
	fc.mu.Lock()
	fc.connected = true
	fc.mu.Unlock()
	ts := int64(100_000)
	res, err := s.FetchMessages(t.Context(), api.FetchMessagesParams{ThreadID: "700", Limit: 100, BeforeTSMS: &ts})
	if err != nil {
		t.Fatal(err)
	}
	if calls != maxFetchPages || len(res.Messages) != maxFetchPages || !res.HasMore {
		t.Fatalf("calls %d messages %d has_more %v, want %d pages of one message and more to come", calls, len(res.Messages), res.HasMore, maxFetchPages)
	}
}

func TestFetchMessagesCanonicalisesTheThreadID(t *testing.T) {
	s, _ := openTest(t, newFakeClient(nil))
	s.processTable(upsertTable(histThread, false, 1000, 2000), modeInitial)
	for _, id := range []string{"700", "0700", "+700"} {
		res, err := s.FetchMessages(t.Context(), api.FetchMessagesParams{ThreadID: id, Limit: 10})
		if err != nil || len(res.Messages) != 2 {
			t.Fatalf("%q: %+v %v", id, res, err)
		}
	}
	for _, id := range []string{"-5", "0", "abc", "", "7e2", " 700"} {
		_, err := s.FetchMessages(t.Context(), api.FetchMessagesParams{ThreadID: id, Limit: 10})
		wantCode(t, err, api.CodeThreadNotFound)
	}
}

func TestCanonicalThreadID(t *testing.T) {
	cases := map[string]string{"333": "333", "0333": "333", "+333": "333", "00333": "333", "9223372036854775807": "9223372036854775807"}
	for in, want := range cases {
		key, canon, ok := canonicalThreadID(in)
		if !ok || canon != want || key <= 0 {
			t.Fatalf("%q -> %d %q %v", in, key, canon, ok)
		}
	}
	for _, in := range []string{"", "0", "-1", "+0", "abc", "3 3", "0x14d", "1e3", "9223372036854775808", " 333", "333 ", "３３３"} {
		if _, canon, ok := canonicalThreadID(in); ok {
			t.Fatalf("%q must be refused, got %q", in, canon)
		}
	}
}
