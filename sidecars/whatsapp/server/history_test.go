package server

import (
	"errors"
	"fmt"
	"sync/atomic"
	"testing"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waCommon"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/proto/waHistorySync"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
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

func TestFetchMessagesMatchesRequestByChatWhenSessionDiffers(t *testing.T) {
	h, c := startPaired(t, true)
	seedMessages(h, c, 2)
	onDemandResponder(c, 3, waHistorySync.Conversation_COMPLETE_BUT_MORE_MESSAGES_REMAIN_ON_PRIMARY, func(string) string { return "OTHER" })
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
