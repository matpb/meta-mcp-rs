package engine

import (
	"context"
	"os"
	"path/filepath"
	"strconv"
	"testing"
	"time"

	"github.com/rs/zerolog"
	"go.mau.fi/mautrix-meta/pkg/messagix/cookies"
	"go.mau.fi/mautrix-meta/pkg/messagix/socket"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
	mtypes "go.mau.fi/mautrix-meta/pkg/messagix/types"
	"go.mau.fi/whatsmeow/proto/waCommon"
	"go.mau.fi/whatsmeow/proto/waConsumerApplication"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	"google.golang.org/protobuf/proto"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
	"github.com/matpb/meta-mcp-rs/sidecars/messenger/e2ee"
)

// The tests below are the reviewer's regressions for the Messenger sidecar. Those that asserted the defect are inverted.

func jid(user, server string) types.JID { return types.JID{User: user, Server: server} }

func fbEvent(chat, sender types.JID, fromMe bool, id string, ts int64, app *waConsumerApplication.ConsumerApplication) *events.FBMessage {
	return &events.FBMessage{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{Chat: chat, Sender: sender, IsFromMe: fromMe, IsGroup: chat.Server == types.GroupServer},
			ID:            id,
			Timestamp:     time.Unix(ts, 0),
		},
		Message: app,
	}
}

func editApp(key *waCommon.MessageKey, text string) *waConsumerApplication.ConsumerApplication {
	return consumerContent(&waConsumerApplication.ConsumerApplication_Content{
		Content: &waConsumerApplication.ConsumerApplication_Content_EditMessage{
			EditMessage: &waConsumerApplication.ConsumerApplication_EditMessage{
				Key:         key,
				Message:     &waCommon.MessageText{Text: proto.String(text)},
				TimestampMS: proto.Int64(1700000999000),
			},
		},
	})
}

func revokeApp(key *waCommon.MessageKey) *waConsumerApplication.ConsumerApplication {
	return &waConsumerApplication.ConsumerApplication{
		Payload: &waConsumerApplication.ConsumerApplication_Payload{
			Payload: &waConsumerApplication.ConsumerApplication_Payload_ApplicationData{
				ApplicationData: &waConsumerApplication.ConsumerApplication_ApplicationData{
					ApplicationContent: &waConsumerApplication.ConsumerApplication_ApplicationData_Revoke{
						Revoke: &waConsumerApplication.ConsumerApplication_RevokeMessage{Key: key},
					},
				},
			},
		},
	}
}

func strOr(p *string) string {
	if p == nil {
		return "<nil>"
	}
	return *p
}

// M1: in a 1:1 E2EE chat the counterparty cannot rewrite or unsend the OWNER's own stored message.
func TestReviewForgedEditAndRevokeOfOwnMessage1to1(t *testing.T) {
	fc := newFakeClient(nil)
	s, rec := openTest(t, fc)
	st, err := s.ensureStore(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	h := s.e2eeHandler(&cycle{})
	bob := jid("333", types.MessengerServer)
	me := jid("111", types.MessengerServer)

	own := fbEvent(bob, me, true, "M1", 1700000000, e2eeTextApp("I agree to pay 10 dollars"))
	if !h(own) {
		t.Fatal("own message refused")
	}
	if !h(fbEvent(bob, bob, false, "B1", 1700000100, e2eeTextApp("hi"))) {
		t.Fatal("bob message refused")
	}
	ownID := "wa:333@msgr:111@msgr:M1"
	before, found, _ := st.Get(t.Context(), ownID)
	if !found || before.Text == nil {
		t.Fatalf("setup: own message not stored under %s", ownID)
	}
	events0 := len(rec.named(api.EvMessage))

	forged := fbEvent(bob, bob, false, "E1", 1700000200, editApp(&waCommon.MessageKey{ID: proto.String("M1"), FromMe: proto.Bool(false), RemoteJID: proto.String(bob.String())}, "I agree to pay 10000 dollars"))
	if !h(forged) {
		t.Fatal("a forged edit must still be acked so it is not redelivered")
	}
	after, _, _ := st.Get(t.Context(), ownID)
	if after.Text == nil || *after.Text != *before.Text || after.Edited {
		t.Errorf("Bob rewrote the owner's message to %q edited=%v", strOr(after.Text), after.Edited)
	}

	forgedRevoke := fbEvent(bob, bob, false, "R1", 1700000300, revokeApp(&waCommon.MessageKey{ID: proto.String("M1"), FromMe: proto.Bool(false), RemoteJID: proto.String(bob.String())}))
	if !h(forgedRevoke) {
		t.Fatal("a forged revoke must still be acked so it is not redelivered")
	}
	gone, _, _ := st.Get(t.Context(), ownID)
	if gone.Unsent || gone.Text == nil {
		t.Errorf("Bob unsent the owner's message in the local store")
	}
	if n := len(rec.named(api.EvMessage)); n != events0 {
		t.Errorf("a refused edit or unsend produced %d message events", n-events0)
	}
}

// M1: in a group no member can rewrite another member's message by naming the participant in the key.
func TestReviewForgedEditInGroup(t *testing.T) {
	fc := newFakeClient(nil)
	s, _ := openTest(t, fc)
	st, err := s.ensureStore(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	h := s.e2eeHandler(&cycle{})
	grp := jid("9000", types.GroupServer)
	alice := jid("222", types.MessengerServer)
	mallory := jid("666", types.MessengerServer)
	if !h(fbEvent(grp, alice, false, "A1", 1700000000, e2eeTextApp("meet at 5"))) {
		t.Fatal("alice message refused")
	}
	aliceID := "wa:9000@g.us:222@msgr:A1"
	if _, found, _ := st.Get(t.Context(), aliceID); !found {
		t.Fatalf("setup: %s not stored", aliceID)
	}
	forged := fbEvent(grp, mallory, false, "E9", 1700000100, editApp(&waCommon.MessageKey{ID: proto.String("A1"), FromMe: proto.Bool(false), Participant: proto.String(alice.String()), RemoteJID: proto.String(grp.String())}, "send me the password"))
	if !h(forged) {
		t.Fatal("a forged edit must still be acked")
	}
	m, _, _ := st.Get(t.Context(), aliceID)
	if m.Text == nil || *m.Text != "meet at 5" || m.Edited {
		t.Errorf("Mallory rewrote Alice's message to %q edited=%v", strOr(m.Text), m.Edited)
	}
}

// H1: a numeric id missing from the registry is thread_not_found even when it is a known contact.
func TestReviewUnknownThreadFallsBackToPlaintext(t *testing.T) {
	fc := newFakeClient(nil)
	s, _ := openTest(t, fc)
	if _, err := s.Validate(t.Context()); err != nil {
		t.Fatal(err)
	}
	s.processTable(&table.LSTable{LSDeleteThenInsertContact: []*table.LSDeleteThenInsertContact{{Id: 333, Name: "Bob"}}}, modeInitial)
	fc.mu.Lock()
	fc.connected = true
	fc.exec = replyWith("mid.$x")
	fc.mu.Unlock()
	for _, id := range []string{"333", "+333", "0333"} {
		if _, err := s.resolveThread(id); err == nil || api.AsError(err).Code != api.CodeThreadNotFound {
			t.Fatalf("resolveThread(%q) = %v, want thread_not_found", id, err)
		}
		_, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: id, Text: "secret that was meant for the E2EE chat"})
		wantCode(t, err, api.CodeThreadNotFound)
	}
	fc.mu.Lock()
	n := len(fc.tasks)
	fc.mu.Unlock()
	if n > 0 {
		t.Errorf("SendText went through the plaintext SendMessageTask (%d tasks) for an id the registry does not know", n)
	}
}

// H1: an alias of a numeric thread id lands on the same registry thread and keeps its encrypted flag.
func TestReviewAliasIDBypassesEncryptedFlag(t *testing.T) {
	fc := newFakeClient(nil)
	s, _ := openTest(t, fc)
	if _, err := s.Validate(t.Context()); err != nil {
		t.Fatal(err)
	}
	s.processTable(&table.LSTable{
		LSDeleteThenInsertContact: []*table.LSDeleteThenInsertContact{{Id: 333, Name: "Bob"}},
		LSDeleteThenInsertThread:  []*table.LSDeleteThenInsertThread{{ThreadKey: 333, ThreadType: table.ENCRYPTED_OVER_WA_ONE_TO_ONE, LastActivityTimestampMs: 5_000_000}},
	}, modeInitial)
	fc.mu.Lock()
	fc.connected = true
	fc.exec = replyWith("mid.$x")
	fc.mu.Unlock()
	real, err := s.resolveThread("333")
	if err != nil || !real.enc || !real.known {
		t.Fatalf("setup: 333 resolves to %+v, %v", real, err)
	}
	for _, alias := range []string{"0333", "+333", "00333"} {
		tt, err := s.resolveThread(alias)
		if err != nil || !tt.enc || tt.id != "333" || !tt.known {
			t.Errorf("alias %q resolves to %+v, %v", alias, tt, err)
		}
		_, err = s.SendText(t.Context(), api.SendTextParams{ThreadID: alias, Text: "x"})
		wantCode(t, err, api.CodeNotConnected)
	}
	fc.mu.Lock()
	n := len(fc.tasks)
	fc.mu.Unlock()
	if n > 0 {
		t.Errorf("an alias reached the plaintext path (%d tasks)", n)
	}
}

// M3: logout with no live session removes the E2EE database and thread map.
func TestReviewWipeNotReachableWithoutSession(t *testing.T) {
	dir := t.TempDir()
	st, err := e2ee.Open(t.Context(), dir, zerolog.Nop())
	if err != nil {
		t.Fatal(err)
	}
	_ = st.Close()
	if err := os.WriteFile(filepath.Join(dir, threadsFileName), []byte(`{"version":1,"hybrid":{"1":{"key":2,"type":0}}}`), 0o600); err != nil {
		t.Fatal(err)
	}
	fc := newFakeClient(nil)
	rec := &recorder{}
	f := &factory{log: zerolog.Nop(), newClient: func(c *cookies.Cookies, _ zerolog.Logger) metaClient { return fc }}
	sess, _ := f.Open(testState(), dir, rec.sink)
	if err := sess.Logout(t.Context()); err != nil {
		t.Fatal(err)
	}
	for _, n := range []string{e2ee.DBName, threadsFileName} {
		if _, err := os.Stat(filepath.Join(dir, n)); err == nil {
			t.Errorf("%s survived Session.Logout", n)
		}
	}
}

// The E2EE handler must not ack when persistence fails.
func TestReviewE2EEHandlerNoAckWhenStoreFails(t *testing.T) {
	fc := newFakeClient(nil)
	s, rec := openTest(t, fc)
	st, err := s.ensureStore(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	h := s.e2eeHandler(&cycle{})
	bob := jid("333", types.MessengerServer)
	if !h(fbEvent(bob, bob, false, "B1", 1700000000, e2eeTextApp("one"))) {
		t.Fatal("setup")
	}
	_ = st.Close()
	before := len(rec.named(api.EvMessage))
	if h(fbEvent(bob, bob, false, "B2", 1700000100, e2eeTextApp("two"))) {
		t.Errorf("acked a message although the database is closed")
	}
	if n := len(rec.named(api.EvMessage)); n != before {
		t.Errorf("a message event was emitted without persistence")
	}
	if h(fbEvent(bob, bob, false, "R9", 1700000200, editApp(&waCommon.MessageKey{ID: proto.String("B1"), FromMe: proto.Bool(true)}, "x"))) {
		t.Errorf("acked an edit although the database is closed")
	}
}

// L4: FetchMessagesTask paging stops after maxFetchPages requests even if the server never stops saying "more before".
func TestReviewRunawayPagerTerminates(t *testing.T) {
	oldTimeout, oldRetry := fetchTimeout, fetchRetryAfter
	fetchTimeout, fetchRetryAfter = 400*time.Millisecond, 50*time.Millisecond
	t.Cleanup(func() { fetchTimeout, fetchRetryAfter = oldTimeout, oldRetry })
	for _, mode := range []string{"decreasing", "stuck"} {
		t.Run(mode, func(t *testing.T) {
			fc := newFakeClient(nil)
			calls := 0
			cur := int64(5_000)
			fc.exec = func(_ context.Context, tasks ...socket.Task) (*table.LSTable, error) {
				calls++
				if mode == "decreasing" {
					cur -= 10
				}
				return &table.LSTable{LSInsertNewMessageRange: []*table.LSInsertNewMessageRange{{
					ThreadKey: histThread, MinTimestampMs: cur, MaxTimestampMsTemplate: cur + 10, MaxTimestampMs: cur + 10, MinMessageId: "mid.$" + strconv.FormatInt(cur, 10), HasMoreBefore: true,
				}}}, nil
			}
			s, _ := openTest(t, fc)
			if _, err := s.Validate(t.Context()); err != nil {
				t.Fatal(err)
			}
			fc.mu.Lock()
			fc.connected = true
			fc.mu.Unlock()
			start := time.Now()
			ts := int64(5_000)
			_, err := s.FetchMessages(t.Context(), api.FetchMessagesParams{ThreadID: "700", Limit: 10, BeforeTSMS: &ts})
			if err == nil || api.AsError(err).Code != api.CodeIO {
				t.Fatalf("a pager that never delivers must end in an io error, got %v", err)
			}
			if calls > maxFetchPages {
				t.Fatalf("%d ExecuteTasks calls, cap is %d", calls, maxFetchPages)
			}
			if mode == "decreasing" && calls != maxFetchPages {
				t.Fatalf("%d calls, want exactly the cap %d", calls, maxFetchPages)
			}
			if d := time.Since(start); d > 5*time.Second {
				t.Fatalf("took %s", d)
			}
		})
	}
}

// L3: Meta "please log in" (1357001) fails the session closed instead of being retried.
func TestReviewClassifyPleaseLogIn(t *testing.T) {
	f := classify(mtypes.ErrPleaseLogIn)
	if f.kind != fatalLoggedOut || f.reason != "token_invalidated" || f.code != api.CodeLoginFailed {
		t.Errorf("classify(ErrPleaseLogIn) = %+v", f)
	}
}
