package engine

import (
	"context"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"go.mau.fi/mautrix-meta/pkg/messagix/socket"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
	"go.mau.fi/mautrix-meta/pkg/metaid"
	"go.mau.fi/mautrix-meta/pkg/msgconv/mediadl"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waConsumerApplication"
	"go.mau.fi/whatsmeow/proto/waMsgApplication"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
	"github.com/matpb/meta-mcp-rs/sidecars/messenger/normalize"
)

// sendSession is a connected session that knows the threads of threadsTable.
func sendSession(t *testing.T) (*session, *fakeClient) {
	t.Helper()
	fc := newFakeClient(nil)
	s, _ := openTest(t, fc)
	if _, err := s.Validate(t.Context()); err != nil {
		t.Fatal(err)
	}
	s.processTable(threadsTable(), modeInitial)
	fc.mu.Lock()
	fc.connected = true
	fc.mu.Unlock()
	return s, fc
}

func replaceTable(otid, mid string) *table.LSTable {
	return &table.LSTable{LSReplaceOptimsiticMessage: []*table.LSReplaceOptimsiticMessage{{OfflineThreadingId: otid, MessageId: mid}}}
}

func wantCode(t *testing.T, err error, code string) *api.Error {
	t.Helper()
	var ae *api.Error
	if !errors.As(err, &ae) || ae.Code != code {
		t.Fatalf("err %v, want code %s", err, code)
	}
	return ae
}

func lastSend(t *testing.T, fc *fakeClient) *socket.SendMessageTask {
	t.Helper()
	fc.mu.Lock()
	defer fc.mu.Unlock()
	for i := len(fc.tasks) - 1; i >= 0; i-- {
		if st, ok := fc.tasks[i].(*socket.SendMessageTask); ok {
			return st
		}
	}
	t.Fatal("no SendMessageTask executed")
	return nil
}

func replyWith(mid string) func(context.Context, ...socket.Task) (*table.LSTable, error) {
	return func(_ context.Context, tasks ...socket.Task) (*table.LSTable, error) {
		st := tasks[0].(*socket.SendMessageTask)
		return replaceTable(strconv.FormatInt(st.Otid, 10), mid), nil
	}
}

func TestSendTextSuccess(t *testing.T) {
	s, fc := sendSession(t)
	fc.exec = replyWith("mid.$abc")
	res, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: "222", Text: "hello"})
	if err != nil {
		t.Fatal(err)
	}
	if res.MessageID != "fb:mid.$abc" || res.TS < time.Now().Unix()-5 {
		t.Fatalf("result %+v", res)
	}
	task := lastSend(t, fc)
	if task.ThreadId != 222 || task.Text != "hello" || task.SendType != table.TEXT || task.SyncGroup != 1 ||
		task.Source != table.MESSENGER_INBOX_IN_THREAD || task.InitiatingSource != table.FACEBOOK_INBOX || task.ReplyMetaData != nil || task.Otid == 0 {
		t.Fatalf("task %+v", task)
	}
	if s.sends.size() != 0 {
		t.Fatal("waiter not cleaned up")
	}
}

func TestSendTextOutcomeArrivesOverSocket(t *testing.T) {
	s, fc := sendSession(t)
	fc.exec = func(_ context.Context, tasks ...socket.Task) (*table.LSTable, error) {
		otid := strconv.FormatInt(tasks[0].(*socket.SendMessageTask).Otid, 10)
		go func() {
			time.Sleep(30 * time.Millisecond)
			s.onSendOutcomes([]api.SendOutcome{{OTID: "unrelated", MessageID: "fb:other"}, {OTID: otid, MessageID: "fb:mid.$late"}})
		}()
		return &table.LSTable{}, nil
	}
	res, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: "222", Text: "hi"})
	if err != nil || res.MessageID != "fb:mid.$late" {
		t.Fatalf("res %+v err %v", res, err)
	}
}

func TestSendTextRejectedNeverLeaksServerText(t *testing.T) {
	s, fc := sendSession(t)
	fc.exec = func(_ context.Context, tasks ...socket.Task) (*table.LSTable, error) {
		otid := strconv.FormatInt(tasks[0].(*socket.SendMessageTask).Otid, 10)
		return &table.LSTable{LSMarkOptimisticMessageFailed: []*table.LSMarkOptimisticMessageFailed{{OTID: otid, Message: "secret server reason"}}}, nil
	}
	_, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: "222", Text: "hi"})
	ae := wantCode(t, err, api.CodeSendFailed)
	if strings.Contains(ae.Msg, "secret") || ae.Msg != "facebook rejected the message" {
		t.Fatalf("message %q", ae.Msg)
	}
}

func TestSendTextUntaggedFailureFailsFast(t *testing.T) {
	s, fc := sendSession(t)
	fc.exec = func(context.Context, ...socket.Task) (*table.LSTable, error) {
		return &table.LSTable{LSHandleFailedTask: []*table.LSHandleFailedTask{{Message: "nope"}}}, nil
	}
	start := time.Now()
	_, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: "222", Text: "hi"})
	wantCode(t, err, api.CodeSendFailed)
	if time.Since(start) > time.Second {
		t.Fatal("should not wait for the outcome timeout")
	}
}

func TestSendTextTaggedFailureOfOtherOTIDDoesNotFailThisSend(t *testing.T) {
	s, fc := sendSession(t)
	fc.exec = func(_ context.Context, tasks ...socket.Task) (*table.LSTable, error) {
		otid := strconv.FormatInt(tasks[0].(*socket.SendMessageTask).Otid, 10)
		go func() {
			time.Sleep(30 * time.Millisecond)
			s.onSendOutcomes([]api.SendOutcome{{OTID: otid, MessageID: "fb:mid.$mine"}})
		}()
		return &table.LSTable{
			LSMarkOptimisticMessageFailed: []*table.LSMarkOptimisticMessageFailed{{OTID: "123", Message: "other"}},
			LSHandleFailedTask:            []*table.LSHandleFailedTask{{OTID: "456", Message: "other"}},
		}, nil
	}
	res, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: "222", Text: "hi"})
	if err != nil || res.MessageID != "fb:mid.$mine" {
		t.Fatalf("res %+v err %v", res, err)
	}
}

func TestSendTextSharedDeadlineCoversExecAndWait(t *testing.T) {
	old := sendOutcomeWait
	sendOutcomeWait = 200 * time.Millisecond
	t.Cleanup(func() { sendOutcomeWait = old })
	s, fc := sendSession(t)
	fc.exec = func(ctx context.Context, _ ...socket.Task) (*table.LSTable, error) {
		<-ctx.Done()
		return nil, ctx.Err()
	}
	start := time.Now()
	_, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: "222", Text: "hi"})
	wantCode(t, err, api.CodeSendUnknown)
	if d := time.Since(start); d > time.Second {
		t.Fatalf("took %v, want about the shared 200ms budget", d)
	}
	fc.exec = func(ctx context.Context, _ ...socket.Task) (*table.LSTable, error) {
		time.Sleep(150 * time.Millisecond)
		return &table.LSTable{}, nil
	}
	start = time.Now()
	_, err = s.SendText(t.Context(), api.SendTextParams{ThreadID: "222", Text: "hi"})
	wantCode(t, err, api.CodeSendUnknown)
	if d := time.Since(start); d > 400*time.Millisecond {
		t.Fatalf("took %v, exec time must count against the same budget", d)
	}
}

func TestSendTextTimeoutIsUnknown(t *testing.T) {
	old := sendOutcomeWait
	sendOutcomeWait = 60 * time.Millisecond
	t.Cleanup(func() { sendOutcomeWait = old })
	s, fc := sendSession(t)
	fc.exec = func(context.Context, ...socket.Task) (*table.LSTable, error) { return &table.LSTable{}, nil }
	_, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: "222", Text: "hi"})
	wantCode(t, err, api.CodeSendUnknown)
	if s.sends.size() != 0 {
		t.Fatal("waiter not cleaned up")
	}
}

func TestSendTextExecErrorIsUnknownAndNotRetried(t *testing.T) {
	s, fc := sendSession(t)
	calls := 0
	fc.exec = func(context.Context, ...socket.Task) (*table.LSTable, error) {
		calls++
		return nil, errors.New("connection reset")
	}
	_, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: "222", Text: "hi"})
	wantCode(t, err, api.CodeSendUnknown)
	if calls != 1 || s.sends.size() != 0 {
		t.Fatalf("calls %d pending %d", calls, s.sends.size())
	}
}

func TestSendTextReplyMetadata(t *testing.T) {
	s, fc := sendSession(t)
	fc.exec = replyWith("mid.$r")
	s.hist.put(api.Message{ID: "fb:mid.$orig", ThreadID: "222", Sender: api.Sender{ID: "222"}, Attachments: []api.Attachment{}}, 1000, true)
	if _, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: "222", Text: "re", ReplyTo: "fb:mid.$orig"}); err != nil {
		t.Fatal(err)
	}
	rm := lastSend(t, fc).ReplyMetaData
	if rm == nil || rm.ReplyMessageId != "mid.$orig" || rm.ReplySourceType != 1 || rm.ReplyType != 0 || rm.ReplySender != 222 {
		t.Fatalf("reply %+v", rm)
	}
}

func TestSendTextValidation(t *testing.T) {
	s, fc := sendSession(t)
	fc.exec = replyWithEmptySearch("mid.$x")
	cases := []struct {
		name string
		p    api.SendTextParams
		code string
	}{
		{"unknown thread", api.SendTextParams{ThreadID: "999999", Text: "x"}, api.CodeThreadNotFound},
		{"non numeric thread", api.SendTextParams{ThreadID: "abc", Text: "x"}, api.CodeThreadNotFound},
		{"too long", api.SendTextParams{ThreadID: "222", Text: strings.Repeat("a", 20001)}, api.CodeBadParams},
		{"wa id on fb thread", api.SendTextParams{ThreadID: "222", Text: "x", ReplyTo: "wa:222@msgr:222@msgr:ABC"}, api.CodeBadParams},
		{"garbage id", api.SendTextParams{ThreadID: "222", Text: "x", ReplyTo: "nonsense"}, api.CodeBadParams},
	}
	for _, c := range cases {
		_, err := s.SendText(t.Context(), c.p)
		if ae := wantCode(t, err, c.code); c.code == api.CodeBadParams && c.name == "wa id on fb thread" && ae.Msg != "message id does not belong to this thread" {
			t.Errorf("%s: message %q", c.name, ae.Msg)
		}
	}
	if _, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: "222", Text: strings.Repeat("é", 20000)}); err != nil {
		t.Fatalf("20000 runes must pass: %v", err)
	}
	if n := countTasks[*socket.SendMessageTask](fc); n != 1 {
		t.Fatalf("%d sends executed, want only the valid send", n)
	}
}

// A contact id without a registry thread could be an encrypted chat that is not listed yet, so it is never sent to.
func TestSendTextKnownContactWithoutThreadIsNotFound(t *testing.T) {
	s, fc := sendSession(t)
	fc.exec = replyWithEmptySearch("mid.$c")
	s.names.set(777, "New Friend")
	_, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: "777", Text: "hi"})
	wantCode(t, err, api.CodeThreadNotFound)
	if n := countTasks[*socket.SendMessageTask](fc); n != 0 {
		t.Fatalf("%d sends executed for an unknown thread", n)
	}
}

func TestSendTextNotConnectedWritesNothing(t *testing.T) {
	s, fc := sendSession(t)
	fc.connected = false
	_, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: "222", Text: "hi"})
	wantCode(t, err, api.CodeNotConnected)
	fc.mu.Lock()
	defer fc.mu.Unlock()
	if len(fc.tasks) != 0 {
		t.Fatal("a task was executed")
	}
}

func TestSendTextEncryptedThreadWithoutClient(t *testing.T) {
	s, fc := sendSession(t)
	_, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: "9000", Text: "hi"})
	wantCode(t, err, api.CodeNotConnected)
	fc.mu.Lock()
	defer fc.mu.Unlock()
	if len(fc.tasks) != 0 {
		t.Fatal("an encrypted send must not use the Messenger socket")
	}
}

func TestSendTextLoggedOut(t *testing.T) {
	s, _ := sendSession(t)
	s.mu.Lock()
	s.loggedOut = true
	s.mu.Unlock()
	_, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: "222", Text: "hi"})
	wantCode(t, err, api.CodeNotLoggedIn)
}

func TestSendMediaValidation(t *testing.T) {
	s, fc := sendSession(t)
	dir := t.TempDir()
	big := filepath.Join(dir, "big.png")
	if err := os.WriteFile(big, make([]byte, maxImageBytes+1), 0o600); err != nil {
		t.Fatal(err)
	}
	_, err := s.SendMedia(t.Context(), api.SendMediaParams{ThreadID: "222", Path: big, Mime: "image/png", Kind: "image"})
	if ae := wantCode(t, err, api.CodeBadParams); ae.Msg != "file too large for messenger" {
		t.Fatalf("msg %q", ae.Msg)
	}
	small := filepath.Join(dir, "a.txt")
	if err := os.WriteFile(small, []byte("hi"), 0o600); err != nil {
		t.Fatal(err)
	}
	_, err = s.SendMedia(t.Context(), api.SendMediaParams{ThreadID: "222", Path: small, Mime: "text/plain", Kind: "file"})
	wantCode(t, err, api.CodeSendFailed)
	_, err = s.SendMedia(t.Context(), api.SendMediaParams{ThreadID: "9000", Path: small, Mime: "text/plain", Kind: "file"})
	if ae := wantCode(t, err, api.CodeUnsupported); ae.Msg != "encrypted threads accept images only in v1" {
		t.Fatalf("msg %q", ae.Msg)
	}
	fc.mu.Lock()
	defer fc.mu.Unlock()
	if len(fc.tasks) != 0 {
		t.Fatal("no send task may run without an upload")
	}
}

func TestSendReactionAndRemoval(t *testing.T) {
	s, fc := sendSession(t)
	fc.exec = func(context.Context, ...socket.Task) (*table.LSTable, error) { return &table.LSTable{}, nil }
	if err := s.SendReaction(t.Context(), api.SendReactionParams{ThreadID: "222", MessageID: "fb:mid.$m", Emoji: "❤️"}); err != nil {
		t.Fatal(err)
	}
	if err := s.SendReaction(t.Context(), api.SendReactionParams{ThreadID: "222", MessageID: "fb:mid.$m", Emoji: ""}); err != nil {
		t.Fatal(err)
	}
	fc.mu.Lock()
	defer fc.mu.Unlock()
	add, rm := fc.tasks[0].(*socket.SendReactionTask), fc.tasks[1].(*socket.SendReactionTask)
	if add.ThreadKey != 222 || add.MessageID != "mid.$m" || add.ActorID != testMe || add.Reaction != "❤" || add.SyncGroup != 1 || add.SendAttribution != table.MESSENGER_INBOX_IN_THREAD {
		t.Fatalf("add %+v", add)
	}
	if rm.Reaction != "" {
		t.Fatalf("removal %+v", rm)
	}
}

func TestSendReactionErrors(t *testing.T) {
	s, fc := sendSession(t)
	err := s.SendReaction(t.Context(), api.SendReactionParams{ThreadID: "222", MessageID: "wa:1@msgr:1@msgr:X", Emoji: "x"})
	wantCode(t, err, api.CodeBadParams)
	fc.exec = func(context.Context, ...socket.Task) (*table.LSTable, error) { return nil, errors.New("boom") }
	err = s.SendReaction(t.Context(), api.SendReactionParams{ThreadID: "222", MessageID: "fb:mid.$m", Emoji: "x"})
	wantCode(t, err, api.CodeSendFailed)
	fc.connected = false
	err = s.SendReaction(t.Context(), api.SendReactionParams{ThreadID: "222", MessageID: "fb:mid.$m", Emoji: "x"})
	wantCode(t, err, api.CodeNotConnected)
}

func TestMarkReadTask(t *testing.T) {
	s, fc := sendSession(t)
	fc.exec = func(context.Context, ...socket.Task) (*table.LSTable, error) { return &table.LSTable{}, nil }
	up := int64(1_234_000)
	if err := s.MarkRead(t.Context(), api.MarkReadParams{ThreadID: "222", UpToTSMS: &up}); err != nil {
		t.Fatal(err)
	}
	before := time.Now().UnixMilli()
	if err := s.MarkRead(t.Context(), api.MarkReadParams{ThreadID: "4000"}); err != nil {
		t.Fatal(err)
	}
	fc.mu.Lock()
	defer fc.mu.Unlock()
	a, b := fc.tasks[0].(*socket.ThreadMarkReadTask), fc.tasks[1].(*socket.ThreadMarkReadTask)
	if a.ThreadId != 222 || a.LastReadWatermarkTs != up || a.SyncGroup != 1 {
		t.Fatalf("a %+v", a)
	}
	if b.ThreadId != 4000 || b.LastReadWatermarkTs < before {
		t.Fatalf("b %+v", b)
	}
}

func TestSetTypingRouting(t *testing.T) {
	s, fc := sendSession(t)
	if err := s.SetTyping(t.Context(), api.SetTypingParams{ThreadID: "222", On: true}); err != nil {
		t.Fatal(err)
	}
	wantCode(t, s.SetTyping(t.Context(), api.SetTypingParams{ThreadID: "9000", On: true}), api.CodeNotConnected)
	fc.connected = false
	wantCode(t, s.SetTyping(t.Context(), api.SetTypingParams{ThreadID: "222", On: true}), api.CodeNotConnected)
	wantCode(t, s.SetTyping(t.Context(), api.SetTypingParams{ThreadID: "1", On: true}), api.CodeThreadNotFound)
}

func TestTypingTask(t *testing.T) {
	dm := typingTask(threadTarget{fbKey: 222, tt: table.ONE_TO_ONE, known: true, entry: threadEntry{Kind: api.KindDM}}, true)
	if dm.ThreadKey != 222 || dm.IsGroupThread != 0 || dm.IsTyping != 1 || dm.SyncGroup != 1 || dm.ThreadType != int64(table.ONE_TO_ONE) || dm.Attribution != 0 {
		t.Fatalf("dm %+v", dm)
	}
	grp := typingTask(threadTarget{fbKey: 4000, tt: table.GROUP_THREAD, known: true, entry: threadEntry{Kind: api.KindGroup}}, false)
	if grp.IsGroupThread != 1 || grp.IsTyping != 0 {
		t.Fatalf("group %+v", grp)
	}
}

func TestChatJIDFor(t *testing.T) {
	cases := []struct {
		e    threadEntry
		want string
	}{
		{threadEntry{ID: "5", Kind: api.KindDM}, "5@msgr"},
		{threadEntry{ID: "9", Kind: api.KindGroup}, "9@g.us"},
		{threadEntry{ID: "9", Kind: api.KindGroup, WAServer: types.GroupServer}, "9@g.us"},
		{threadEntry{ID: "5", Kind: api.KindDM, WAServer: types.DefaultUserServer}, "5@s.whatsapp.net"},
	}
	for _, c := range cases {
		if got := chatJIDFor(c.e).String(); got != c.want {
			t.Errorf("%+v -> %s want %s", c.e, got, c.want)
		}
	}
}

func TestResolveThreadRouting(t *testing.T) {
	s, _ := sendSession(t)
	enc, err := s.resolveThread("9000")
	if err != nil || !enc.enc || enc.chat.String() != "9000@g.us" || enc.fbKey != 5000 {
		t.Fatalf("encrypted %+v err %v", enc, err)
	}
	dm, err := s.resolveThread("222")
	if err != nil || dm.enc || dm.fbKey != 222 || dm.tt != table.ONE_TO_ONE {
		t.Fatalf("dm %+v err %v", dm, err)
	}
	_, err = s.resolveThread("-5")
	wantCode(t, err, api.CodeThreadNotFound)
}

func TestCheckMessageIDSides(t *testing.T) {
	s, _ := sendSession(t)
	enc, _ := s.resolveThread("9000")
	if _, err := s.checkMessageID(enc, "wa:9000@g.us:1@msgr:ABC"); err != nil {
		t.Fatal(err)
	}
	for _, id := range []string{"fb:mid.$x", "wa:1234@g.us:1@msgr:ABC"} {
		wantCode(t, errOf(s.checkMessageID(enc, id)), api.CodeBadParams)
	}
	dm, _ := s.resolveThread("222")
	wantCode(t, errOf(s.checkMessageID(dm, "wa:222@msgr:222@msgr:ABC")), api.CodeBadParams)
	s.hist.put(api.Message{ID: "fb:mid.$in4000", ThreadID: "4000"}, 5, true)
	wantCode(t, errOf(s.checkMessageID(dm, "fb:mid.$in4000")), api.CodeBadParams)
}

func errOf(_ metaid.ParsedMessageID, err error) error { return err }

func TestClassifyE2EESend(t *testing.T) {
	cases := []struct {
		err  error
		code string
	}{
		{whatsmeow.ErrNotConnected, api.CodeNotConnected},
		{whatsmeow.ErrNotLoggedIn, api.CodeNotConnected},
		{whatsmeow.ErrServerReturnedError, api.CodeSendFailed},
		{errors.Join(errors.New("x"), whatsmeow.ErrServerReturnedError), api.CodeSendFailed},
		{whatsmeow.ErrMessageTimedOut, api.CodeSendUnknown},
		{context.DeadlineExceeded, api.CodeSendUnknown},
		{errors.New("failed to send message node"), api.CodeSendUnknown},
	}
	for _, c := range cases {
		if got := classifyE2EESend(c.err); got.Code != c.code {
			t.Errorf("%v -> %s want %s", c.err, got.Code, c.code)
		}
	}
}

func TestWAKey(t *testing.T) {
	me := "111"
	mine := metaid.ParsedWAMessageID{Chat: types.JID{User: "222", Server: types.MessengerServer}, Sender: types.JID{User: "111", Server: types.MessengerServer}, ID: "M1"}
	k := waKey(mine, me)
	if !k.GetFromMe() || k.GetParticipant() != "" || k.GetID() != "M1" || k.GetRemoteJID() != "222@msgr" {
		t.Fatalf("dm own %+v", k)
	}
	group := metaid.ParsedWAMessageID{Chat: types.JID{User: "9", Server: types.GroupServer}, Sender: types.JID{User: "333", Server: types.MessengerServer}, ID: "M2"}
	k = waKey(group, me)
	if k.GetFromMe() || k.GetParticipant() != "333@msgr" || k.GetRemoteJID() != "9@g.us" {
		t.Fatalf("group other %+v", k)
	}
}

func TestGroupUnread(t *testing.T) {
	mk := func(id string, ts int64, me bool) api.Message {
		return api.Message{ID: id, TS: ts, Sender: api.Sender{IsMe: me}}
	}
	msgs := []api.Message{
		mk("wa:9@g.us:333@msgr:A", 100, false),
		mk("wa:9@g.us:333@msgr:B", 200, false),
		mk("wa:9@g.us:444@msgr:C", 150, false),
		mk("wa:9@g.us:111@msgr:D", 160, true),
		mk("wa:5@msgr:5@msgr:E", 170, false),
		mk("fb:ignored", 10, false),
	}
	got := groupUnread(msgs, nil)
	if len(got[types.JID{User: "333", Server: types.MessengerServer}]) != 2 || len(got[types.JID{User: "444", Server: types.MessengerServer}]) != 1 {
		t.Fatalf("group senders %v", got)
	}
	if ids := got[types.JID{}]; len(ids) != 1 || ids[0] != "E" {
		t.Fatalf("dm bucket %v", got)
	}
	upTo := int64(150_000)
	got = groupUnread(msgs, &upTo)
	if n := len(got[types.JID{User: "333", Server: types.MessengerServer}]); n != 1 {
		t.Fatalf("watermark kept %d", n)
	}
}

func TestReactionAppShape(t *testing.T) {
	key := waKey(metaid.ParsedWAMessageID{Chat: types.JID{User: "9", Server: types.GroupServer}, Sender: types.JID{User: "333", Server: types.MessengerServer}, ID: "M"}, "111")
	app := e2eeReactionApp(key, "", 1234)
	r := app.GetPayload().GetContent().GetReactionMessage()
	if r == nil || r.GetText() != "" || r.GetSenderTimestampMS() != 1234 || r.GetKey().GetID() != "M" {
		t.Fatalf("reaction %+v", r)
	}
}

// normalizeSent runs a built ConsumerApplication through the incoming-message normaliser.
func normalizeSent(t *testing.T, app *waConsumerApplication.ConsumerApplication, meta *waMsgApplication.MessageApplication_Metadata) api.Message {
	t.Helper()
	chat := types.JID{User: "9000", Server: types.GroupServer}
	own := types.JID{User: "111", Server: types.MessengerServer}
	evt := &events.FBMessage{
		Info:          types.MessageInfo{MessageSource: types.MessageSource{Chat: chat, Sender: own, IsFromMe: true, IsGroup: true}, ID: "ID1", Timestamp: time.Unix(1700000000, 0)},
		Message:       app,
		FBApplication: &waMsgApplication.MessageApplication{Metadata: meta},
	}
	res := normalize.FBMessage(evt, normalize.E2EEContext{Me: testMe})
	if res.Message == nil {
		t.Fatal("not a message")
	}
	return *res.Message
}

func TestE2EETextAppAndReplyRoundTrip(t *testing.T) {
	reply := metaid.ParsedWAMessageID{Chat: types.JID{User: "9000", Server: types.GroupServer}, Sender: types.JID{User: "333", Server: types.MessengerServer}, ID: "Q1"}
	m := normalizeSent(t, e2eeTextApp("hello"), e2eeReplyMeta(reply))
	if m.Text == nil || *m.Text != "hello" || m.Kind != api.MsgText || !m.Sender.IsMe || !m.Encrypted {
		t.Fatalf("message %+v", m)
	}
	want := string(metaid.MakeWAMessageID(reply.Chat, reply.Sender, "Q1"))
	if m.ReplyTo == nil || *m.ReplyTo != want {
		t.Fatalf("reply %v want %s", m.ReplyTo, want)
	}
}

func TestRecordSentLandsInStoreAndFetch(t *testing.T) {
	s, _ := sendSession(t)
	chat := types.JID{User: "9000", Server: types.GroupServer}
	tgt, err := s.resolveThread("9000")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := s.ensureStore(t.Context()); err != nil {
		t.Fatal(err)
	}
	own := types.JID{User: "111", Server: types.MessengerServer}
	s.recordSent(tgt, own, "SENT1", time.Unix(1700000500, 0), e2eeTextApp("from me"), nil)
	res, err := s.FetchMessages(t.Context(), api.FetchMessagesParams{ThreadID: "9000", Limit: 10})
	if err != nil || len(res.Messages) != 1 {
		t.Fatalf("fetch %+v err %v", res, err)
	}
	m := res.Messages[0]
	if m.ID != string(metaid.MakeWAMessageID(chat, own, "SENT1")) || !m.Sender.IsMe || m.Text == nil || *m.Text != "from me" {
		t.Fatalf("message %+v", m)
	}
}

func cachedAttachmentMsg(s *session, url string, expires *int64) {
	mime := "image/png"
	s.hist.put(api.Message{
		ID: "fb:mid.$att", ThreadID: "222", Kind: api.MsgMedia, Reactions: []api.Reaction{},
		Attachments: []api.Attachment{{ID: "0", Kind: api.AttImage, Mime: &mime, URL: &url, ExpiresAt: expires}},
	}, 5_000, true)
}

func stubDownload(t *testing.T, fn func(url string) (io.ReadCloser, error)) *[]string {
	t.Helper()
	old := downloadMedia
	var urls []string
	downloadMedia = func(_ context.Context, _, url string, _ int64) (int64, io.ReadCloser, error) {
		urls = append(urls, url)
		rc, err := fn(url)
		return 0, rc, err
	}
	t.Cleanup(func() { downloadMedia = old })
	return &urls
}

func TestDownloadAttachmentWritesFile(t *testing.T) {
	s, _ := sendSession(t)
	cachedAttachmentMsg(s, "https://cdn.example/a.png?oe=1", nil)
	stubDownload(t, func(string) (io.ReadCloser, error) { return io.NopCloser(strings.NewReader("hello")), nil })
	dest := filepath.Join(t.TempDir(), "a.png")
	res, err := s.DownloadAttachment(t.Context(), api.DownloadParams{ThreadID: "222", MessageID: "fb:mid.$att", AttachmentID: "0", DestPath: dest})
	if err != nil {
		t.Fatal(err)
	}
	if res.Path != dest || res.Mime != "image/png" || res.Bytes != 5 || res.SHA256 != "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824" {
		t.Fatalf("result %+v", res)
	}
	if st, _ := os.Stat(dest); st.Mode().Perm() != 0o600 {
		t.Fatalf("mode %v", st.Mode().Perm())
	}
}

func TestDownloadAttachmentLookups(t *testing.T) {
	s, _ := sendSession(t)
	cachedAttachmentMsg(s, "https://cdn.example/a.png", nil)
	stubDownload(t, func(string) (io.ReadCloser, error) { return nil, errors.New("must not be called") })
	dest := filepath.Join(t.TempDir(), "x")
	_, err := s.DownloadAttachment(t.Context(), api.DownloadParams{ThreadID: "222", MessageID: "fb:mid.$none", AttachmentID: "0", DestPath: dest})
	wantCode(t, err, api.CodeMessageNotFound)
	_, err = s.DownloadAttachment(t.Context(), api.DownloadParams{ThreadID: "222", MessageID: "fb:mid.$att", AttachmentID: "7", DestPath: dest})
	if ae := wantCode(t, err, api.CodeBadParams); ae.Msg != "unknown attachment" {
		t.Fatalf("msg %q", ae.Msg)
	}
	s.hist.put(api.Message{ID: "fb:mid.$nourl", ThreadID: "222", Attachments: []api.Attachment{{ID: "0", Kind: api.AttFile}}}, 6_000, true)
	_, err = s.DownloadAttachment(t.Context(), api.DownloadParams{ThreadID: "222", MessageID: "fb:mid.$nourl", AttachmentID: "0", DestPath: dest})
	if ae := wantCode(t, err, api.CodeUnsupported); ae.Msg != "attachment has no downloadable media" {
		t.Fatalf("msg %q", ae.Msg)
	}
}

func TestDownloadAttachmentRefreshesOnForbidden(t *testing.T) {
	s, fc := sendSession(t)
	cachedAttachmentMsg(s, "https://cdn.example/old", nil)
	fc.exec = func(_ context.Context, tasks ...socket.Task) (*table.LSTable, error) {
		if _, ok := tasks[0].(*socket.FetchMessagesTask); !ok {
			t.Errorf("unexpected task %T", tasks[0])
		}
		cachedAttachmentMsg(s, "https://cdn.example/new", nil)
		return &table.LSTable{}, nil
	}
	urls := stubDownload(t, func(u string) (io.ReadCloser, error) {
		if strings.HasSuffix(u, "/old") {
			return nil, mediadl.ErrForbidden
		}
		return io.NopCloser(strings.NewReader("fresh")), nil
	})
	dest := filepath.Join(t.TempDir(), "a")
	res, err := s.DownloadAttachment(t.Context(), api.DownloadParams{ThreadID: "222", MessageID: "fb:mid.$att", AttachmentID: "0", DestPath: dest})
	if err != nil || res.Bytes != 5 {
		t.Fatalf("res %+v err %v", res, err)
	}
	if len(*urls) != 2 || !strings.HasSuffix((*urls)[1], "/new") {
		t.Fatalf("urls %v", *urls)
	}
}

func TestDownloadAttachmentExpiredAfterOneRefresh(t *testing.T) {
	s, fc := sendSession(t)
	cachedAttachmentMsg(s, "https://cdn.example/old", nil)
	refreshes := 0
	fc.exec = func(context.Context, ...socket.Task) (*table.LSTable, error) {
		refreshes++
		return &table.LSTable{}, nil
	}
	urls := stubDownload(t, func(string) (io.ReadCloser, error) { return nil, mediadl.ErrForbidden })
	dest := filepath.Join(t.TempDir(), "a")
	_, err := s.DownloadAttachment(t.Context(), api.DownloadParams{ThreadID: "222", MessageID: "fb:mid.$att", AttachmentID: "0", DestPath: dest})
	ae := wantCode(t, err, api.CodeAttachmentExpired)
	if refreshes != 1 || len(*urls) != 2 || strings.Contains(ae.Msg, "cdn.example") {
		t.Fatalf("refreshes %d downloads %d msg %q", refreshes, len(*urls), ae.Msg)
	}
	if _, statErr := os.Stat(dest); statErr == nil {
		t.Fatal("destination must not exist")
	}
}

func TestDownloadAttachmentNearExpiryRefreshesFirst(t *testing.T) {
	s, fc := sendSession(t)
	soon := time.Now().Unix() + 10
	cachedAttachmentMsg(s, "https://cdn.example/old", &soon)
	refreshes := 0
	fc.exec = func(context.Context, ...socket.Task) (*table.LSTable, error) {
		refreshes++
		later := time.Now().Unix() + 3600
		cachedAttachmentMsg(s, "https://cdn.example/new", &later)
		return &table.LSTable{}, nil
	}
	urls := stubDownload(t, func(string) (io.ReadCloser, error) { return io.NopCloser(strings.NewReader("ok")), nil })
	dest := filepath.Join(t.TempDir(), "a")
	if _, err := s.DownloadAttachment(t.Context(), api.DownloadParams{ThreadID: "222", MessageID: "fb:mid.$att", AttachmentID: "0", DestPath: dest}); err != nil {
		t.Fatal(err)
	}
	if refreshes != 1 || len(*urls) != 1 || !strings.HasSuffix((*urls)[0], "/new") {
		t.Fatalf("refreshes %d urls %v", refreshes, *urls)
	}
}

func TestDownloadAttachmentTooLargeAndErrors(t *testing.T) {
	s, _ := sendSession(t)
	cachedAttachmentMsg(s, "https://cdn.example/big", nil)
	dest := filepath.Join(t.TempDir(), "a")
	params := api.DownloadParams{ThreadID: "222", MessageID: "fb:mid.$att", AttachmentID: "0", DestPath: dest}
	stubDownload(t, func(string) (io.ReadCloser, error) { return nil, mediadl.ErrTooLargeFile })
	_, err := s.DownloadAttachment(t.Context(), params)
	if ae := wantCode(t, err, api.CodeIO); ae.Msg != "attachment exceeds 100 MiB" {
		t.Fatalf("msg %q", ae.Msg)
	}
	stubDownload(t, func(string) (io.ReadCloser, error) {
		return nil, errors.New("Get https://cdn.example/big?token=secret: reset")
	})
	_, err = s.DownloadAttachment(t.Context(), params)
	if ae := wantCode(t, err, api.CodeIO); strings.Contains(ae.Msg, "cdn") || strings.Contains(ae.Msg, "secret") {
		t.Fatalf("msg %q leaks the url", ae.Msg)
	}
}

func TestE2EEHandlerRedeliveryStillEmitsAndPanicReturnsFalse(t *testing.T) {
	fc := newFakeClient(nil)
	s, rec := openTest(t, fc)
	if _, err := s.ensureStore(t.Context()); err != nil {
		t.Fatal(err)
	}
	evt := &events.FBMessage{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{Chat: types.JID{User: "333", Server: types.MessengerServer}, Sender: types.JID{User: "333", Server: types.MessengerServer}},
			ID:            "IN1", Timestamp: time.Unix(1700000000, 0),
		},
		Message: e2eeTextApp("hello"),
	}
	h := s.e2eeHandler(&cycle{})
	if !h(evt) || !h(evt) {
		t.Fatal("both deliveries must be acked")
	}
	if n := len(rec.named(api.EvMessage)); n != 2 {
		t.Fatalf("%d message events, want 2 (redelivery re-emits)", n)
	}
	if h((*events.ConnectFailure)(nil)) {
		t.Fatal("a panicking handler must not ack")
	}
}
