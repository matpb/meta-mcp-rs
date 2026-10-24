package engine

import (
	"context"
	"strconv"
	"testing"

	"go.mau.fi/mautrix-meta/pkg/messagix/socket"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

const histThread = int64(700)

func upsertTable(thread int64, hasMoreBefore bool, ts ...int64) *table.LSTable {
	tbl := &table.LSTable{}
	for _, v := range ts {
		tbl.LSUpsertMessage = append(tbl.LSUpsertMessage, &table.LSUpsertMessage{
			MessageId: "mid.$" + strconv.FormatInt(v, 10), ThreadKey: thread, TimestampMs: v, SenderId: alice, Text: "m" + strconv.FormatInt(v, 10),
		})
	}
	lo, hi := ts[0], ts[0]
	for _, v := range ts {
		lo, hi = min(lo, v), max(hi, v)
	}
	tbl.LSInsertNewMessageRange = []*table.LSInsertNewMessageRange{{
		ThreadKey: thread, MinTimestampMs: lo, MaxTimestampMs: hi, MinMessageId: "mid.$" + strconv.FormatInt(lo, 10),
		MaxMessageId: "mid.$" + strconv.FormatInt(hi, 10), HasMoreBefore: hasMoreBefore,
	}}
	return tbl
}

func ids(msgs []api.Message) []string {
	out := make([]string, len(msgs))
	for i, m := range msgs {
		out[i] = m.ID
	}
	return out
}

func equalIDs(got []api.Message, want ...string) bool {
	g := ids(got)
	if len(g) != len(want) {
		return false
	}
	for i := range g {
		if g[i] != want[i] {
			return false
		}
	}
	return true
}

func TestFetchMessagesFromCache(t *testing.T) {
	s, rec := openTest(t, newFakeClient(nil))
	s.processTable(upsertTable(histThread, false, 1000, 2000, 3000, 4000, 5000), modeInitial)

	if got := rec.named(api.EvHistory); len(got) != 1 {
		t.Fatalf("want one history event, got %d", len(got))
	} else if h := got[0].data.(api.HistoryEvent); h.ThreadID != "700" || len(h.Messages) != 5 || !h.Complete {
		t.Fatalf("history %+v", h)
	}
	if len(rec.named(api.EvMessage)) != 0 {
		t.Fatal("initial messages must not be emitted as live messages")
	}

	res, err := s.FetchMessages(t.Context(), api.FetchMessagesParams{ThreadID: "700", Limit: 2})
	if err != nil {
		t.Fatal(err)
	}
	if !equalIDs(res.Messages, "fb:mid.$5000", "fb:mid.$4000") || !res.HasMore {
		t.Fatalf("page 1 %v more=%v", ids(res.Messages), res.HasMore)
	}
	res, err = s.FetchMessages(t.Context(), api.FetchMessagesParams{ThreadID: "700", Limit: 2, BeforeID: "fb:mid.$4000"})
	if err != nil {
		t.Fatal(err)
	}
	if !equalIDs(res.Messages, "fb:mid.$3000", "fb:mid.$2000") || !res.HasMore {
		t.Fatalf("page 2 %v more=%v", ids(res.Messages), res.HasMore)
	}
	res, err = s.FetchMessages(t.Context(), api.FetchMessagesParams{ThreadID: "700", Limit: 2, BeforeID: "fb:mid.$2000"})
	if err != nil {
		t.Fatal(err)
	}
	if !equalIDs(res.Messages, "fb:mid.$1000") || res.HasMore {
		t.Fatalf("page 3 %v more=%v", ids(res.Messages), res.HasMore)
	}
	ts := int64(3500)
	res, err = s.FetchMessages(t.Context(), api.FetchMessagesParams{ThreadID: "700", Limit: 10, BeforeTSMS: &ts})
	if err != nil {
		t.Fatal(err)
	}
	if !equalIDs(res.Messages, "fb:mid.$3000", "fb:mid.$2000", "fb:mid.$1000") || res.HasMore {
		t.Fatalf("ts page %v more=%v", ids(res.Messages), res.HasMore)
	}
}

func TestFetchMessagesErrors(t *testing.T) {
	s, _ := openTest(t, newFakeClient(nil))
	for _, id := range []string{"abc", "0", "-5", ""} {
		_, err := s.FetchMessages(t.Context(), api.FetchMessagesParams{ThreadID: id, Limit: 5})
		if ae, ok := err.(*api.Error); !ok || ae.Code != api.CodeThreadNotFound {
			t.Fatalf("thread %q: %v", id, err)
		}
	}
	_, err := s.FetchMessages(t.Context(), api.FetchMessagesParams{ThreadID: "700", Limit: 5})
	if ae, ok := err.(*api.Error); !ok || ae.Code != api.CodeNotConnected {
		t.Fatalf("not connected: %v", err)
	}
	s.processTable(upsertTable(histThread, false, 1000), modeInitial)
	_, err = s.FetchMessages(t.Context(), api.FetchMessagesParams{ThreadID: "700", Limit: 5, BeforeID: "fb:mid.$nope"})
	if ae, ok := err.(*api.Error); !ok || ae.Code != api.CodeMessageNotFound {
		t.Fatalf("unknown before_id: %v", err)
	}
}

func TestFetchMessagesRemoteCollector(t *testing.T) {
	fc := newFakeClient(nil)
	var task *socket.FetchMessagesTask
	fc.exec = func(_ context.Context, tasks ...socket.Task) (*table.LSTable, error) {
		task = tasks[0].(*socket.FetchMessagesTask)
		return upsertTable(histThread, false, 500, 600), nil
	}
	s, rec := openTest(t, fc)
	if _, err := s.Validate(t.Context()); err != nil {
		t.Fatal(err)
	}
	fc.mu.Lock()
	fc.connected = true
	fc.mu.Unlock()
	s.processTable(upsertTable(histThread, true, 1000, 2000), modeInitial)
	before := len(rec.named(api.EvHistory))

	res, err := s.FetchMessages(t.Context(), api.FetchMessagesParams{ThreadID: "700", Limit: 5, BeforeID: "fb:mid.$1000"})
	if err != nil {
		t.Fatal(err)
	}
	if !equalIDs(res.Messages, "fb:mid.$600", "fb:mid.$500") || res.HasMore {
		t.Fatalf("remote page %v more=%v", ids(res.Messages), res.HasMore)
	}
	if task == nil || task.ThreadKey != histThread || task.ReferenceTimestampMs != 1000 || task.ReferenceMessageId != "mid.$1000" || task.SyncGroup != 1 || task.Direction != 0 || task.Cursor != "cursor" {
		t.Fatalf("task %+v", task)
	}
	if got := len(rec.named(api.EvHistory)); got != before {
		t.Fatalf("collected pages must not be re-emitted as history (%d -> %d)", before, got)
	}

	// The pages are cached now, so the same request needs no further task.
	task = nil
	res, err = s.FetchMessages(t.Context(), api.FetchMessagesParams{ThreadID: "700", Limit: 5, BeforeID: "fb:mid.$1000"})
	if err != nil {
		t.Fatal(err)
	}
	if task != nil || !equalIDs(res.Messages, "fb:mid.$600", "fb:mid.$500") || res.HasMore {
		t.Fatalf("cached page %v more=%v task=%v", ids(res.Messages), res.HasMore, task)
	}
}

func TestFetchMessagesRemoteFollowsRange(t *testing.T) {
	fc := newFakeClient(nil)
	calls := 0
	fc.exec = func(_ context.Context, tasks ...socket.Task) (*table.LSTable, error) {
		calls++
		task := tasks[0].(*socket.FetchMessagesTask)
		if calls == 1 {
			if task.ReferenceTimestampMs != 3000 {
				t.Errorf("first reference %d", task.ReferenceTimestampMs)
			}
			return upsertTable(histThread, true, 2000), nil
		}
		if task.ReferenceTimestampMs != 2000 || task.ReferenceMessageId != "mid.$2000" {
			t.Errorf("second reference %+v", task)
		}
		return upsertTable(histThread, false, 1000, 1500), nil
	}
	s, _ := openTest(t, fc)
	if _, err := s.Validate(t.Context()); err != nil {
		t.Fatal(err)
	}
	fc.mu.Lock()
	fc.connected = true
	fc.mu.Unlock()
	ts := int64(3000)
	res, err := s.FetchMessages(t.Context(), api.FetchMessagesParams{ThreadID: "700", Limit: 10, BeforeTSMS: &ts})
	if err != nil {
		t.Fatal(err)
	}
	if !equalIDs(res.Messages, "fb:mid.$2000", "fb:mid.$1500", "fb:mid.$1000") || res.HasMore || calls != 2 {
		t.Fatalf("pages %v more=%v calls=%d", ids(res.Messages), res.HasMore, calls)
	}
}

func TestFetchMessagesEncryptedUsesStore(t *testing.T) {
	s, _ := openTest(t, newFakeClient(nil))
	s.processTable(threadsTable(), modeInitial)
	st, err := s.ensureStore(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	for i, ts := range []int64{1000, 2000, 3000} {
		m := api.Message{
			ID: "wa:9000@g.us:333@msgr:" + strconv.Itoa(i), ThreadID: "9000", TS: ts / 1000,
			Attachments: []api.Attachment{}, Reactions: []api.Reaction{}, Encrypted: true, Kind: api.MsgText,
		}
		if err := st.Put(t.Context(), m, ts, nil); err != nil {
			t.Fatal(err)
		}
	}
	res, err := s.FetchMessages(t.Context(), api.FetchMessagesParams{ThreadID: "9000", Limit: 2})
	if err != nil {
		t.Fatal(err)
	}
	if len(res.Messages) != 2 || !res.HasMore || res.Messages[0].TS != 3 {
		t.Fatalf("store page %+v more=%v", res.Messages, res.HasMore)
	}
}

func TestMsgCacheCapAndCoverage(t *testing.T) {
	c := newMsgCache()
	for i := 1; i <= cacheCap+20; i++ {
		m := api.Message{ID: "fb:m" + strconv.Itoa(i), ThreadID: "9"}
		c.put(m, int64(i), true)
	}
	c.mu.Lock()
	n := len(c.threads["9"].msgs)
	covMin := c.threads["9"].covMin
	c.mu.Unlock()
	if n != cacheCap {
		t.Fatalf("cache holds %d, want %d", n, cacheCap)
	}
	if covMin != 21 {
		t.Fatalf("coverage must start at the oldest kept message, got %d", covMin)
	}
	if _, _, ok := c.get("fb:m1"); ok {
		t.Fatal("oldest message must be dropped")
	}
}

func TestLiveMessageEventsAndEdits(t *testing.T) {
	s, rec := openTest(t, newFakeClient(nil))
	s.processTable(threadsTable(), modeInitial)
	live := &table.LSTable{LSInsertMessage: []*table.LSInsertMessage{
		{MessageId: "mid.$live", ThreadKey: alice, TimestampMs: 4_000_000, SenderId: alice, Text: "yo"},
		{MessageId: "mid.$mine", ThreadKey: alice, TimestampMs: 4_000_001, SenderId: testMe, Text: "sup"},
	}}
	s.processTable(live, modeLive)
	msgs := rec.named(api.EvMessage)
	if len(msgs) != 2 {
		t.Fatalf("want 2 message events, got %d", len(msgs))
	}
	if e := msgs[0].data.(api.MessageEvent); e.Direction != "in" || e.Message.ID != "fb:mid.$live" {
		t.Fatalf("first %+v", e)
	}
	if e := msgs[1].data.(api.MessageEvent); e.Direction != "out" || !e.Message.Sender.IsMe {
		t.Fatalf("second %+v", e)
	}
	s.processTable(live, modeLive)
	if len(rec.named(api.EvMessage)) != 2 {
		t.Fatal("a repeated identical message must not be emitted twice")
	}
	if snap, _ := s.reg.snapshot("222"); *snap.LastMessagePreview != "sup" || *snap.LastMessageTS != 4000 {
		t.Fatalf("thread must follow live messages: %+v", snap)
	}

	s.processTable(&table.LSTable{LSEditMessage: []*table.LSEditMessage{{MessageID: "mid.$live", Text: "yo!"}}}, modeLive)
	ev := rec.named(api.EvMessage)
	last := ev[len(ev)-1].data.(api.MessageEvent).Message
	if !last.Edited || last.Text == nil || *last.Text != "yo!" || last.ThreadID != "222" || last.Sender.ID != "222" {
		t.Fatalf("edit of a cached message %+v", last)
	}
	s.processTable(&table.LSTable{LSEditMessage: []*table.LSEditMessage{{MessageID: "mid.$unknown", Text: "x"}}}, modeLive)
	ev = rec.named(api.EvMessage)
	last = ev[len(ev)-1].data.(api.MessageEvent).Message
	if !last.Edited || last.ID != "fb:mid.$unknown" || last.Sender.ID != "" || last.ThreadID != "" {
		t.Fatalf("edit of an unknown message %+v", last)
	}

	s.processTable(&table.LSTable{LSDeleteThenInsertMessage: []*table.LSDeleteThenInsertMessage{{ThreadKey: alice, MessageId: "mid.$live", IsUnsent: true}}}, modeLive)
	ev = rec.named(api.EvMessage)
	last = ev[len(ev)-1].data.(api.MessageEvent).Message
	if !last.Unsent || last.Text != nil || last.Kind != api.MsgUnsupported || len(last.Attachments) != 0 {
		t.Fatalf("unsend %+v", last)
	}

	s.processTable(&table.LSTable{LSUpsertReaction: []*table.LSUpsertReaction{{ThreadKey: alice, MessageId: "mid.$mine", ActorId: alice, Reaction: "x", TimestampMs: 5_000_000}}}, modeLive)
	re := rec.named(api.EvReaction)
	if len(re) != 1 {
		t.Fatalf("want 1 reaction, got %d", len(re))
	}
	m, _, _ := s.hist.get("fb:mid.$mine")
	if len(m.Reactions) != 1 || m.Reactions[0].SenderID != "222" {
		t.Fatalf("cached reactions %+v", m.Reactions)
	}
}
