package server

import (
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waCommon"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/proto/waHistorySync"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/normalize"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/store"
)

// seedMessages stores n incoming DM messages M0..M(n-1) with ts 1700000100+100*i.
func seedMessages(h *harness, c *fakeClient, n int) {
	h.t.Helper()
	for i := 0; i < n; i++ {
		h.incoming(c, peerPN, fmt.Sprintf("M%d", i), int64(1700000100+100*i), fmt.Sprintf("msg %d", i))
	}
	h.consume("thread")
	h.consume("message")
}

func ids(msgs []map[string]any) string {
	var out []string
	for _, m := range msgs {
		out = append(out, m["id"].(string))
	}
	return fmt.Sprint(out)
}

func TestFetchMessagesPagesFromStore(t *testing.T) {
	h, c := startPaired(t, false)
	seedMessages(h, c, 5)
	msgs, more := h.fetch(peerPN, 2)
	if ids(msgs) != "[M4 M3]" || !more {
		t.Fatalf("page 1 %s more=%v", ids(msgs), more)
	}
	res := h.mustCall("fetch_messages", map[string]any{"thread_id": peerPN, "limit": 2, "before_ts_ms": 1700000300 * 1000, "before_id": "M2"})
	if got := ids(toMaps(res["messages"])); got != "[M1 M0]" || res["has_more"] != false {
		t.Fatalf("page 2 %s more=%v", got, res["has_more"])
	}
	res = h.mustCall("fetch_messages", map[string]any{"thread_id": peerPN, "limit": 3, "before_ts_ms": 1700000300 * 1000})
	if got := ids(toMaps(res["messages"])); got != "[M1 M0]" || res["has_more"] != false {
		t.Fatalf("strict ts page %s more=%v", got, res["has_more"])
	}
	h.wantErr(h.call("fetch_messages", map[string]any{"thread_id": "999@s.whatsapp.net", "limit": 2}), api.CodeThreadNotFound)
	h.wantErr(h.call("fetch_messages", map[string]any{"thread_id": peerPN, "limit": 0}), api.CodeBadParams)
	h.wantErr(h.call("fetch_messages", map[string]any{"thread_id": peerPN, "limit": 101}), api.CodeBadParams)
	h.wantErr(h.call("fetch_messages", map[string]any{"limit": 5}), api.CodeBadParams)
}

func toMaps(v any) []map[string]any {
	var out []map[string]any
	for _, x := range v.([]any) {
		out = append(out, x.(map[string]any))
	}
	return out
}

// onDemandResponder answers peer history requests with n older messages.
func onDemandResponder(c *fakeClient, n int, end waHistorySync.Conversation_EndOfHistoryTransferType, session func(string) string) {
	c.sendFn = func(to types.JID, msg *waE2E.Message, extra whatsmeow.SendRequestExtra) (whatsmeow.SendResponse, error) {
		if extra.Peer {
			conv := &waHistorySync.Conversation{ID: ptr(peerPN), EndOfHistoryTransferType: end.Enum()}
			for i := 0; i < n; i++ {
				conv.Messages = append(conv.Messages, webMsg(peerPN, fmt.Sprintf("O%d", i), uint64(1000+i), false, "old"))
			}
			c.deliver(historyEvent(waHistorySync.HistorySync_ON_DEMAND, session(extra.ID), conv))
		}
		return whatsmeow.SendResponse{}, nil
	}
}

func peerRequests(c *fakeClient) []sentMsg {
	var out []sentMsg
	for _, s := range c.sentSnapshot() {
		if s.extra.Peer {
			out = append(out, s)
		}
	}
	return out
}

func TestFetchMessagesOnDemandRequest(t *testing.T) {
	h, c := startPaired(t, true)
	seedMessages(h, c, 3)
	onDemandResponder(c, 5, waHistorySync.Conversation_COMPLETE_ON_DEMAND_SYNC_BUT_MORE_MSG_REMAIN_ON_PRIMARY, func(id string) string { return id })
	msgs, more := h.fetch(peerPN, 10)
	if len(msgs) != 8 || msgs[0]["id"] != "M2" || msgs[7]["id"] != "O0" || !more {
		t.Fatalf("fetch %s more=%v", ids(msgs), more)
	}
	reqs := peerRequests(c)
	if len(reqs) != 1 {
		t.Fatalf("peer requests %d", len(reqs))
	}
	r := reqs[0]
	req := r.msg.GetProtocolMessage().GetPeerDataOperationRequestMessage().GetHistorySyncOnDemandRequest()
	if r.to.String() != selfPN || !r.extra.Peer || r.extra.ID == "" || req.GetChatJID() != peerPN || req.GetOldestMsgID() != "M0" || req.GetOnDemandMsgCount() != 50 || req.GetOldestMsgFromMe() {
		t.Fatalf("request to=%v extra=%+v req=%+v", r.to, r.extra, req)
	}
	if got := h.pending("history"); len(got) != 1 {
		t.Fatalf("on-demand history events %d", len(got))
	}
}

func TestFetchMessagesOnDemandEndOfHistory(t *testing.T) {
	h, c := startPaired(t, true)
	seedMessages(h, c, 3)
	onDemandResponder(c, 2, waHistorySync.Conversation_COMPLETE_AND_NO_MORE_MESSAGE_REMAIN_ON_PRIMARY, func(id string) string { return id })
	msgs, more := h.fetch(peerPN, 10)
	if len(msgs) != 5 || more {
		t.Fatalf("fetch %s more=%v", ids(msgs), more)
	}
	msgs, more = h.fetch(peerPN, 20)
	if len(msgs) != 5 || more || len(peerRequests(c)) != 1 {
		t.Fatalf("second fetch %d more=%v requests=%d", len(msgs), more, len(peerRequests(c)))
	}
}

func TestFetchMessagesIgnoresAnAnswerOfAnotherSession(t *testing.T) {
	h, c := startPaired(t, true, withTiming(func(*timing) {}))
	seedMessages(h, c, 2)
	onDemandResponder(c, 3, waHistorySync.Conversation_COMPLETE_BUT_MORE_MESSAGES_REMAIN_ON_PRIMARY, func(string) string { return "OTHER" })
	msgs, _ := h.fetch(peerPN, 10)
	if len(msgs) != 2 {
		t.Fatalf("an answer for another session must not finish the wait: %s", ids(msgs))
	}
	if all, _, _ := h.srv.store.FetchMessages(h.srv.ctx, peerPN, 10, 0, ""); len(all) != 5 {
		t.Fatalf("the late answer is still stored: %d rows", len(all))
	}
	if len(h.srv.hist.m) != 0 {
		t.Fatal("a timed out request must not stay in flight")
	}
}

func TestFetchMessagesAnswerWithoutSessionMatchesByChat(t *testing.T) {
	h, c := startPaired(t, true)
	seedMessages(h, c, 2)
	onDemandResponder(c, 3, waHistorySync.Conversation_COMPLETE_BUT_MORE_MESSAGES_REMAIN_ON_PRIMARY, func(string) string { return "" })
	msgs, more := h.fetch(peerPN, 10)
	if len(msgs) != 5 || !more {
		t.Fatalf("fetch %s more=%v", ids(msgs), more)
	}
}

func TestFetchMessagesOneRequestInFlightPerChat(t *testing.T) {
	h, c := startPaired(t, true)
	seedMessages(h, c, 2)
	release := make(chan struct{})
	var requests atomic.Int32
	c.sendFn = func(to types.JID, msg *waE2E.Message, extra whatsmeow.SendRequestExtra) (whatsmeow.SendResponse, error) {
		if extra.Peer {
			requests.Add(1)
			<-release
			conv := &waHistorySync.Conversation{ID: ptr(peerPN), Messages: []*waHistorySync.HistorySyncMsg{webMsg(peerPN, "O1", 1000, false, "old")}}
			c.deliver(historyEvent(waHistorySync.HistorySync_ON_DEMAND, extra.ID, conv))
		}
		return whatsmeow.SendResponse{}, nil
	}
	first := h.send("fetch_messages", map[string]any{"thread_id": peerPN, "limit": 10})
	eventually(t, "first request sent", func() bool { return requests.Load() == 1 })
	second := h.send("fetch_messages", map[string]any{"thread_id": peerPN, "limit": 10})
	eventually(t, "second caller joined", func() bool {
		h.srv.hist.mu.Lock()
		defer h.srv.hist.mu.Unlock()
		w := h.srv.hist.m[peerPN]
		return w != nil && w.joined.Load() == 1
	})
	close(release)
	for _, id := range []int{first, second} {
		res := h.mustOK(h.reply(id))
		if len(res["messages"].([]any)) != 3 {
			t.Fatalf("messages %v", res["messages"])
		}
	}
	if requests.Load() != 1 {
		t.Fatalf("requests %d", requests.Load())
	}
}

func TestFetchMessagesTimeoutKeepsStoreRows(t *testing.T) {
	h, c := startPaired(t, true, withTiming(func(*timing) {}))
	seedMessages(h, c, 2)
	_, _, _ = h.srv.store.UpdateChat(h.srv.ctx, peerPN, func(ch *store.Chat) { ch.EndOfHistory = endMoreRemain })
	msgs, more := h.fetch(peerPN, 10)
	if len(msgs) != 2 || !more {
		t.Fatalf("timeout fetch %s more=%v (EndOfHistory says more remain)", ids(msgs), more)
	}
	h.fetch(peerPN, 10)
	if len(peerRequests(c)) != 2 {
		t.Fatalf("a timed out request must not stay in flight: %d requests", len(peerRequests(c)))
	}
}

func TestFetchMessagesRequestSendFailure(t *testing.T) {
	h, c := startPaired(t, true)
	seedMessages(h, c, 2)
	c.sendFn = func(types.JID, *waE2E.Message, whatsmeow.SendRequestExtra) (whatsmeow.SendResponse, error) {
		return whatsmeow.SendResponse{}, errors.New("socket closed")
	}
	msgs, more := h.fetch(peerPN, 10)
	if len(msgs) != 2 || more {
		t.Fatalf("fetch %s more=%v", ids(msgs), more)
	}
}

func TestFetchMessagesNoRequestWhenDisconnectedOrFull(t *testing.T) {
	h, c := startPaired(t, false)
	seedMessages(h, c, 3)
	h.fetch(peerPN, 10)
	c.mu.Lock()
	c.connected = true
	c.mu.Unlock()
	h.fetch(peerPN, 3)
	h.fetch(peerPN, 2)
	if len(peerRequests(c)) != 0 {
		t.Fatalf("unexpected peer requests: %d", len(peerRequests(c)))
	}
}

func TestFetchMessagesBlockedSendsNoHistoryRequest(t *testing.T) {
	h, c := startPaired(t, true)
	seedMessages(h, c, 2)
	c.deliver(&events.StreamReplaced{})
	msgs, _ := h.fetch(peerPN, 10)
	if len(msgs) != 2 || len(peerRequests(c)) != 0 {
		t.Fatalf("messages %d requests %d", len(msgs), len(peerRequests(c)))
	}
}

func TestFetchMessagesFailedRequestReleasesJoinedCallers(t *testing.T) {
	h, c := startPaired(t, true)
	seedMessages(h, c, 2)
	release := make(chan struct{})
	var requests atomic.Int32
	c.sendFn = func(_ types.JID, _ *waE2E.Message, extra whatsmeow.SendRequestExtra) (whatsmeow.SendResponse, error) {
		if extra.Peer {
			requests.Add(1)
			<-release
			return whatsmeow.SendResponse{}, errors.New("socket closed")
		}
		return whatsmeow.SendResponse{}, nil
	}
	first := h.send("fetch_messages", map[string]any{"thread_id": peerPN, "limit": 10})
	eventually(t, "request sent", func() bool { return requests.Load() == 1 })
	second := h.send("fetch_messages", map[string]any{"thread_id": peerPN, "limit": 10})
	eventually(t, "second caller joined", func() bool {
		h.srv.hist.mu.Lock()
		defer h.srv.hist.mu.Unlock()
		w := h.srv.hist.m[peerPN]
		return w != nil && w.joined.Load() == 1
	})
	close(release)
	for _, id := range []int{first, second} {
		if res := h.mustOK(h.reply(id)); len(res["messages"].([]any)) != 2 {
			t.Fatalf("messages %v", res["messages"])
		}
	}
}

func TestFetchMessagesHasMoreFromEndOfHistoryWhenPageIsFull(t *testing.T) {
	h, c := startPaired(t, false)
	seedMessages(h, c, 2)
	if _, more := h.fetch(peerPN, 2); more {
		t.Fatal("no more rows and no end-of-history hint")
	}
	_, _, _ = h.srv.store.UpdateChat(h.srv.ctx, peerPN, func(ch *store.Chat) { ch.EndOfHistory = endMoreOnDem })
	if _, more := h.fetch(peerPN, 2); !more {
		t.Fatal("the phone reported more history, has_more must be true")
	}
}

func TestHistoryCopyDoesNotRevertEditedMessage(t *testing.T) {
	h, c := startPaired(t, false)
	h.incoming(c, peerPN, "A1", 1700000100, "original")
	c.deliver(msgEvent(peerPN, peerPN, false, "E1", 1700000150, &waE2E.Message{ProtocolMessage: &waE2E.ProtocolMessage{
		Type: waE2E.ProtocolMessage_MESSAGE_EDIT.Enum(), Key: &waCommon.MessageKey{ID: ptr("A1")}, EditedMessage: text("edited"),
	}}))
	h.consume("thread")
	h.consume("message")
	conv := &waHistorySync.Conversation{ID: ptr(peerPN), Messages: []*waHistorySync.HistorySyncMsg{webMsg(peerPN, "A1", 1700000100, false, "original")}}
	if !c.deliver(historyEvent(waHistorySync.HistorySync_RECENT, "", conv)) {
		t.Fatal("refused")
	}
	ev := h.event("history")
	m := ev["messages"].([]any)[0].(map[string]any)
	if m["text"] != "edited" || m["edited"] != true {
		t.Fatalf("history emitted a stale row: %v", m)
	}
}

// Review L5: a response that names another request's session must not finish the wait for this one.
func TestReviewHistoryResolveFallsBackToChat(t *testing.T) {
	hw := histWaiters{m: map[string]*histWait{}}
	w := &histWait{id: "REQ-2", done: make(chan struct{})}
	hw.m[peerPN] = w
	hw.resolve("REQ-1-late-answer-of-an-older-request", []string{peerPN})
	select {
	case <-w.done:
		t.Errorf("a response carrying another request's session id finished the wait for REQ-2")
	case <-time.After(50 * time.Millisecond):
	}
	hw.resolve("REQ-2", nil)
	select {
	case <-w.done:
	case <-time.After(time.Second):
		t.Errorf("the answer for REQ-2 must finish its wait")
	}
	w2 := &histWait{id: "REQ-3", done: make(chan struct{})}
	hw.m[peerPN] = w2
	hw.resolve("", []string{peerPN})
	select {
	case <-w2.done:
	case <-time.After(time.Second):
		t.Errorf("an answer without a session id matches by chat")
	}
}

func bigConversation(chat string, n, size int) *waHistorySync.Conversation {
	big := strings.Repeat("<", size) // json.Marshal writes each "<" as <, 6 bytes
	conv := &waHistorySync.Conversation{ID: ptr(chat), EndOfHistoryTransferType: waHistorySync.Conversation_COMPLETE_AND_NO_MORE_MESSAGE_REMAIN_ON_PRIMARY.Enum()}
	for i := 0; i < n; i++ {
		conv.Messages = append(conv.Messages, webMsg(chat, fmt.Sprintf("B%03d", i), uint64(1700000000+i), false, big))
	}
	return conv
}

// Review L1: a conversation whose batch would pass the frame limit must not abort the rest of the sync.
func TestReviewOversizedHistoryBatchAbortsRestOfSync(t *testing.T) {
	h, c := startPaired(t, false)
	conv := bigConversation(peerPN, 60, 60_000) // about 21 MB once serialised
	small := &waHistorySync.Conversation{ID: ptr(group), Name: ptr("Crew"), Messages: []*waHistorySync.HistorySyncMsg{webMsg(group, "G1", 1700000500, false, "hello crew")}}
	if !c.deliver(historyEvent(waHistorySync.HistorySync_INITIAL_BOOTSTRAP, "", conv, small)) {
		t.Fatal("the handler must accept the sync")
	}
	raw := h.rawEvents("history")
	perThread := map[string][]bool{}
	total := map[string]int{}
	for _, r := range raw {
		if len(r) > normalize.HistoryBatchBytes {
			t.Errorf("a history event of %d bytes passes the %d byte cap", len(r), normalize.HistoryBatchBytes)
		}
		var ev struct {
			ThreadID string            `json:"thread_id"`
			Messages []json.RawMessage `json:"messages"`
			Complete bool              `json:"complete"`
		}
		if err := json.Unmarshal(r, &ev); err != nil {
			t.Fatal(err)
		}
		if len(ev.Messages) > normalize.HistoryBatchMax {
			t.Errorf("a history event of %d messages passes the %d message cap", len(ev.Messages), normalize.HistoryBatchMax)
		}
		perThread[ev.ThreadID] = append(perThread[ev.ThreadID], ev.Complete)
		total[ev.ThreadID] += len(ev.Messages)
	}
	if total[peerPN] != 60 || len(perThread[peerPN]) < 5 || total[group] != 1 {
		t.Fatalf("messages per thread %v, batches %v", total, perThread)
	}
	for _, thread := range []string{peerPN, group} {
		flags := perThread[thread]
		for i, complete := range flags {
			if complete != (i == len(flags)-1) {
				t.Errorf("%s: complete flags %v, only the last batch of a thread is complete", thread, flags)
			}
		}
	}
	names := map[string]bool{}
	for _, th := range h.threads() {
		names[th["id"].(string)] = true
	}
	if !names[peerPN] || !names[group] {
		t.Errorf("threads %v", names)
	}
}

func TestHistoryBatchesAreCappedByCountAndBytes(t *testing.T) {
	h, c := startPaired(t, false)
	conv := &waHistorySync.Conversation{ID: ptr(peerPN), EndOfHistoryTransferType: waHistorySync.Conversation_COMPLETE_AND_NO_MORE_MESSAGE_REMAIN_ON_PRIMARY.Enum()}
	for i := 0; i < 450; i++ {
		conv.Messages = append(conv.Messages, webMsg(peerPN, fmt.Sprintf("H%03d", i), uint64(1700000000+i), false, "short"))
	}
	if !c.deliver(historyEvent(waHistorySync.HistorySync_RECENT, "", conv)) {
		t.Fatal("refused")
	}
	var sizes []int
	for _, r := range h.rawEvents("history") {
		var ev struct {
			Messages []json.RawMessage `json:"messages"`
		}
		_ = json.Unmarshal(r, &ev)
		sizes = append(sizes, len(ev.Messages))
	}
	if fmt.Sprint(sizes) != "[200 200 50]" {
		t.Fatalf("small messages batch by count: %v", sizes)
	}
	h2, c2 := startPaired(t, false)
	if !c2.deliver(historyEvent(waHistorySync.HistorySync_RECENT, "", bigConversation(peerPN, 30, 120_000))) { // 30 x 720 KB
		t.Fatal("refused")
	}
	sizes = nil
	for _, r := range h2.rawEvents("history") {
		var ev struct {
			Messages []json.RawMessage `json:"messages"`
		}
		_ = json.Unmarshal(r, &ev)
		sizes = append(sizes, len(ev.Messages))
		if len(r) > normalize.HistoryBatchBytes {
			t.Errorf("event of %d bytes", len(r))
		}
	}
	if fmt.Sprint(sizes) != "[5 5 5 5 5 5]" {
		t.Fatalf("big messages batch by bytes: %v", sizes)
	}
}

func TestHistoryEmitFailureSkipsTheBatchAndContinues(t *testing.T) {
	h, c := startPaired(t, false)
	giant := strings.Repeat("<", 3_000_000) // 18 MB serialised: over the 16 MiB frame limit on its own
	conv := &waHistorySync.Conversation{ID: ptr(peerPN), Messages: []*waHistorySync.HistorySyncMsg{
		webMsg(peerPN, "S1", 1700000100, false, "before"),
		webMsg(peerPN, "G1", 1700000200, false, giant),
		webMsg(peerPN, "S2", 1700000300, false, "after"),
	}}
	other := &waHistorySync.Conversation{ID: ptr(group), Name: ptr("Crew"), Messages: []*waHistorySync.HistorySyncMsg{webMsg(group, "C1", 1700000400, false, "crew")}}
	if !c.deliver(historyEvent(waHistorySync.HistorySync_INITIAL_BOOTSTRAP, "", conv, other)) {
		t.Fatal("an unwritable batch must not make the handler refuse the whole sync")
	}
	got := map[string][]string{}
	complete := map[string]bool{}
	for _, r := range h.rawEvents("history") {
		var ev struct {
			ThreadID string `json:"thread_id"`
			Messages []struct {
				ID string `json:"id"`
			} `json:"messages"`
			Complete bool `json:"complete"`
		}
		_ = json.Unmarshal(r, &ev)
		for _, m := range ev.Messages {
			got[ev.ThreadID] = append(got[ev.ThreadID], m.ID)
		}
		complete[ev.ThreadID] = complete[ev.ThreadID] || ev.Complete
	}
	if fmt.Sprint(got[peerPN]) != "[S2 S1]" || fmt.Sprint(got[group]) != "[C1]" || !complete[peerPN] || !complete[group] {
		t.Fatalf("history events %v complete %v", got, complete)
	}
	if errs := h.pending("error"); len(errs) != 1 || errs[0]["code"] != api.CodeIO {
		t.Fatalf("the daemon is told about the skipped batch: %v", errs)
	}
	if m, found, _ := h.srv.store.GetMessage(h.srv.ctx, peerPN, "G1"); !found || m.Text == nil || len(*m.Text) != len(giant) {
		t.Fatal("the skipped message stays in the store")
	}

	// when the skipped batch is the last one the thread is still closed with an empty complete batch
	tail := &waHistorySync.Conversation{ID: ptr("14155550222@s.whatsapp.net"), Messages: []*waHistorySync.HistorySyncMsg{
		webMsg("14155550222@s.whatsapp.net", "T1", 1700000100, false, giant),
		webMsg("14155550222@s.whatsapp.net", "T2", 1700000200, false, "newest"),
	}}
	if !c.deliver(historyEvent(waHistorySync.HistorySync_RECENT, "", tail)) {
		t.Fatal("refused")
	}
	var flags []bool
	var sizes []int
	for _, r := range h.rawEvents("history") {
		var ev struct {
			Messages []json.RawMessage `json:"messages"`
			Complete bool              `json:"complete"`
		}
		_ = json.Unmarshal(r, &ev)
		flags, sizes = append(flags, ev.Complete), append(sizes, len(ev.Messages))
	}
	if fmt.Sprint(flags) != "[false true]" || fmt.Sprint(sizes) != "[1 0]" {
		t.Fatalf("flags %v sizes %v", flags, sizes)
	}
}
