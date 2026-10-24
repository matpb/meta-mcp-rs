package server

import (
	"context"
	"fmt"
	"strings"
	"testing"
	"time"

	"go.mau.fi/whatsmeow/proto/waCommon"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/proto/waHistorySync"
	"go.mau.fi/whatsmeow/proto/waSyncAction"
	"go.mau.fi/whatsmeow/proto/waWeb"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
)

const (
	peerPN  = "14155550100@s.whatsapp.net"
	peerLID = "123456789@lid"
	group   = "120363000000000001@g.us"
)

func (h *harness) threads() []map[string]any {
	h.t.Helper()
	res := h.mustCall("list_threads", map[string]any{"limit": 100})
	var out []map[string]any
	for _, x := range res["threads"].([]any) {
		out = append(out, x.(map[string]any))
	}
	return out
}

func (h *harness) fetch(thread string, limit int) ([]map[string]any, bool) {
	h.t.Helper()
	res := h.mustCall("fetch_messages", map[string]any{"thread_id": thread, "limit": limit})
	var out []map[string]any
	for _, x := range res["messages"].([]any) {
		out = append(out, x.(map[string]any))
	}
	return out, res["has_more"].(bool)
}

func TestIncomingLIDDMLandsUnderPhoneThread(t *testing.T) {
	h, c := startPaired(t, false)
	evt := msgEvent(peerLID, peerLID, false, "A1", 1700000100, text("hello"))
	evt.Info.SenderAlt = mustJID(peerPN)
	if !c.deliver(evt) {
		t.Fatal("handler refused the message")
	}
	th, thSeq := h.eventSeq("thread")
	msg, msgSeq := h.eventSeq("message")
	thread := th["thread"].(map[string]any)
	if thread["id"] != peerPN || thread["alt_id"] != peerLID || thread["kind"] != "dm" || thread["name"] != "Alice" || thread["encrypted"] != true {
		t.Fatalf("thread %v", thread)
	}
	parts := thread["participants"].([]any)
	if len(parts) != 2 || parts[1].(map[string]any)["is_me"] != true {
		t.Fatalf("participants %v", parts)
	}
	m := msg["message"].(map[string]any)
	sender := m["sender"].(map[string]any)
	if msg["direction"] != "in" || m["thread_id"] != peerPN || m["text"] != "hello" || sender["id"] != peerPN || sender["name"] != "Alice" || m["kind"] != "text" {
		t.Fatalf("message %v", msg)
	}
	if thSeq > msgSeq {
		t.Fatal("thread event must precede the first message")
	}
	if got := h.pending("thread"); len(got) != 0 {
		t.Fatalf("a second thread event: %v", got)
	}
	all := h.threads()
	if len(all) != 1 || all[0]["unread"] != true || all[0]["last_message_preview"] != "hello" {
		t.Fatalf("threads %v", all)
	}
}

func TestLIDChatMergesIntoPhoneThread(t *testing.T) {
	h, c := startPaired(t, false)
	h.incoming(c, peerLID, "A1", 1700000100, "from lid")
	if all := h.threads(); len(all) != 1 || all[0]["id"] != peerLID {
		t.Fatalf("threads %v", all)
	}
	evt := msgEvent(peerLID, peerLID, false, "A2", 1700000200, text("with phone"))
	evt.Info.SenderAlt = mustJID(peerPN)
	if !c.deliver(evt) {
		t.Fatal("refused")
	}
	all := h.threads()
	if len(all) != 1 || all[0]["id"] != peerPN || all[0]["alt_id"] != peerLID {
		t.Fatalf("threads after merge %v", all)
	}
	msgs, _ := h.fetch(peerPN, 10)
	if len(msgs) != 2 {
		t.Fatalf("merged messages %v", msgs)
	}
	if msgs, _ := h.fetch(peerLID, 10); len(msgs) != 2 {
		t.Fatalf("lookup by the alternate id failed: %v", msgs)
	}
}

func TestLIDResolvedThroughClient(t *testing.T) {
	h, c := startPaired(t, false)
	c.lidToPN[peerLID] = mustJID(peerPN)
	h.incoming(c, peerLID, "A1", 1700000100, "hi")
	if all := h.threads(); len(all) != 1 || all[0]["id"] != peerPN {
		t.Fatalf("threads %v", all)
	}
	if pn, found, _ := h.srv.store.PNForLID(context.Background(), peerLID); !found || pn != peerPN {
		t.Fatalf("mapping not cached: %q %v", pn, found)
	}
}

func TestOwnMessageFromPhoneIsOutgoing(t *testing.T) {
	h, c := startPaired(t, false)
	evt := msgEvent(peerPN, selfLID, true, "O1", 1700000100, text("sent from phone"))
	evt.Info.RecipientAlt = mustJID(peerLID)
	if !c.deliver(evt) {
		t.Fatal("refused")
	}
	h.event("thread")
	msg := h.event("message")
	m := msg["message"].(map[string]any)
	sender := m["sender"].(map[string]any)
	if msg["direction"] != "out" || sender["is_me"] != true || sender["id"] != selfPN || m["thread_id"] != peerPN {
		t.Fatalf("message %v", msg)
	}
	if all := h.threads(); all[0]["unread"] != false {
		t.Fatalf("own message must not be unread: %v", all)
	}
}

func TestEditRevokeAndReaction(t *testing.T) {
	h, c := startPaired(t, false)
	h.incoming(c, peerPN, "A1", 1700000100, "original")
	h.consume("message")

	edit := &waE2E.Message{ProtocolMessage: &waE2E.ProtocolMessage{
		Type: waE2E.ProtocolMessage_MESSAGE_EDIT.Enum(), Key: &waCommon.MessageKey{ID: ptr("A1")}, EditedMessage: text("edited"),
	}}
	if !c.deliver(msgEvent(peerPN, peerPN, false, "E1", 1700000150, edit)) {
		t.Fatal("edit refused")
	}
	m := h.event("message")["message"].(map[string]any)
	if m["id"] != "A1" || m["text"] != "edited" || m["edited"] != true || m["unsent"] != false {
		t.Fatalf("edited message %v", m)
	}

	react := func(emoji string) *waE2E.Message {
		return &waE2E.Message{ReactionMessage: &waE2E.ReactionMessage{Key: &waCommon.MessageKey{ID: ptr("A1")}, Text: ptr(emoji)}}
	}
	c.deliver(msgEvent(peerPN, peerPN, false, "R1", 1700000160, react("👍")))
	ev := h.event("reaction")
	if ev["thread_id"] != peerPN || ev["message_id"] != "A1" || ev["sender_id"] != peerPN || ev["emoji"] != "👍" || ev["ts"] != float64(1700000160) {
		t.Fatalf("reaction %v", ev)
	}
	msgs, _ := h.fetch(peerPN, 10)
	if rs := msgs[0]["reactions"].([]any); len(rs) != 1 || rs[0].(map[string]any)["emoji"] != "👍" {
		t.Fatalf("stored reactions %v", msgs[0]["reactions"])
	}
	c.deliver(msgEvent(peerPN, peerPN, false, "R2", 1700000170, react("")))
	if ev := h.event("reaction"); ev["emoji"] != nil {
		t.Fatalf("removed reaction must carry null: %v", ev)
	}
	msgs, _ = h.fetch(peerPN, 10)
	if rs := msgs[0]["reactions"].([]any); len(rs) != 0 {
		t.Fatalf("reaction not removed %v", rs)
	}

	revoke := &waE2E.Message{ProtocolMessage: &waE2E.ProtocolMessage{
		Type: waE2E.ProtocolMessage_REVOKE.Enum(), Key: &waCommon.MessageKey{ID: ptr("A1")},
	}}
	if !c.deliver(msgEvent(peerPN, peerPN, false, "V1", 1700000200, revoke)) {
		t.Fatal("revoke refused")
	}
	m = h.event("message")["message"].(map[string]any)
	if m["unsent"] != true || m["text"] != nil || len(m["attachments"].([]any)) != 0 {
		t.Fatalf("revoked message %v", m)
	}
	if !c.deliver(msgEvent(peerPN, peerPN, false, "E9", 1700000300, &waE2E.Message{ProtocolMessage: &waE2E.ProtocolMessage{
		Type: waE2E.ProtocolMessage_MESSAGE_EDIT.Enum(), Key: &waCommon.MessageKey{ID: ptr("UNKNOWN")}, EditedMessage: text("x"),
	}})) {
		t.Fatal("edit of an unknown message must be acked")
	}
	if got := h.pending("message"); len(got) != 0 {
		t.Fatalf("unexpected events %v", got)
	}
}

func TestEncryptedReactionAndEdit(t *testing.T) {
	h, c := startPaired(t, false)
	h.incoming(c, peerPN, "A1", 1700000100, "original")
	h.consume("message")
	c.decReaction = func(*events.Message) (*waE2E.ReactionMessage, error) {
		return &waE2E.ReactionMessage{Text: ptr("❤️")}, nil
	}
	enc := &waE2E.Message{EncReactionMessage: &waE2E.EncReactionMessage{TargetMessageKey: &waCommon.MessageKey{ID: ptr("A1")}}}
	if !c.deliver(msgEvent(peerPN, peerPN, false, "R1", 1700000160, enc)) {
		t.Fatal("refused")
	}
	if ev := h.event("reaction"); ev["emoji"] != "❤️" || ev["message_id"] != "A1" {
		t.Fatalf("reaction %v", ev)
	}
	c.decSecret = func(*events.Message) (*waE2E.Message, error) { return text("secret edit"), nil }
	secret := &waE2E.Message{SecretEncryptedMessage: &waE2E.SecretEncryptedMessage{TargetMessageKey: &waCommon.MessageKey{ID: ptr("A1")}}}
	if !c.deliver(msgEvent(peerPN, peerPN, false, "S1", 1700000170, secret)) {
		t.Fatal("refused")
	}
	if m := h.event("message")["message"].(map[string]any); m["text"] != "secret edit" || m["edited"] != true {
		t.Fatalf("edit %v", m)
	}
	c.decReaction, c.decSecret = nil, nil
	if !c.deliver(msgEvent(peerPN, peerPN, false, "R2", 1700000180, enc)) || !c.deliver(msgEvent(peerPN, peerPN, false, "S2", 1700000190, secret)) {
		t.Fatal("undecryptable payloads must still be acked")
	}
	if got := h.pending("reaction"); len(got) != 0 {
		t.Fatalf("events after decrypt failure: %v", got)
	}
}

func TestReceipts(t *testing.T) {
	h, c := startPaired(t, false)
	h.incoming(c, peerPN, "A1", 1700000100, "hello")
	h.consume("message")
	h.consume("thread")
	src := types.MessageSource{Chat: mustJID(peerPN), Sender: mustJID(peerPN)}
	c.deliver(&events.Receipt{MessageSource: src, MessageIDs: []string{"A1"}, Timestamp: time.Unix(1700000999, 0), Type: types.ReceiptTypeRead})
	ev := h.event("receipt")
	if ev["kind"] != "read" || ev["thread_id"] != peerPN || ev["sender_id"] != peerPN || ev["up_to_ts"] != float64(1700000100) {
		t.Fatalf("read receipt %v", ev)
	}
	c.deliver(&events.Receipt{MessageSource: src, MessageIDs: []string{"unknown"}, Timestamp: time.Unix(1700000555, 0), Type: types.ReceiptTypeDelivered})
	if ev := h.event("receipt"); ev["kind"] != "delivered" || ev["up_to_ts"] != float64(1700000555) {
		t.Fatalf("delivered receipt %v", ev)
	}
	c.deliver(&events.Receipt{MessageSource: src, MessageIDs: []string{"A1"}, Timestamp: time.Unix(1700000600, 0), Type: types.ReceiptTypePlayed})
	if ev := h.event("receipt"); ev["kind"] != "read" {
		t.Fatalf("played receipt %v", ev)
	}
	c.deliver(&events.Receipt{MessageSource: src, MessageIDs: []string{"A1"}, Type: types.ReceiptTypeRetry})
	if got := h.pending("receipt"); len(got) != 0 {
		t.Fatalf("retry receipts are ignored: %v", got)
	}
	if h.threads()[0]["unread"] != true {
		t.Fatal("setup: chat should be unread")
	}
	c.deliver(&events.Receipt{MessageSource: types.MessageSource{Chat: mustJID(peerPN), Sender: mustJID(selfPN), IsFromMe: true}, MessageIDs: []string{"A1"}, Timestamp: time.Unix(1700000700, 0), Type: types.ReceiptTypeReadSelf})
	th := h.event("thread")["thread"].(map[string]any)
	if th["unread"] != false {
		t.Fatalf("read-self thread %v", th)
	}
	if h.threads()[0]["unread"] != false {
		t.Fatal("read-self must clear unread")
	}
	c.deliver(&events.Receipt{MessageSource: src, MessageIDs: []string{"A1"}, Timestamp: time.Unix(1, 0), Type: types.ReceiptTypeRead})
	h.consume("receipt")
}

func TestFilteredChatsAreIgnoredButAcked(t *testing.T) {
	h, c := startPaired(t, false)
	for _, chat := range []string{"status@broadcast", "120363111@newsletter", "1234@bot"} {
		if !c.deliver(msgEvent(chat, peerPN, false, "F1", 1700000100, text("x"))) {
			t.Fatalf("%s must be acked", chat)
		}
	}
	if !c.deliver(&events.Receipt{MessageSource: types.MessageSource{Chat: mustJID("status@broadcast"), Sender: mustJID(peerPN)}, MessageIDs: []string{"F1"}, Type: types.ReceiptTypeRead}) {
		t.Fatal("status receipt must be acked")
	}
	for _, name := range []string{"message", "thread", "receipt"} {
		if got := h.pending(name); len(got) != 0 {
			t.Fatalf("%s events for filtered chats: %v", name, got)
		}
	}
	if all := h.threads(); len(all) != 0 {
		t.Fatalf("threads %v", all)
	}
}

func TestStoreFailureMakesHandlerReturnFalse(t *testing.T) {
	h, c := startPaired(t, false)
	h.incoming(c, peerPN, "A1", 1700000100, "ok")
	_ = h.srv.store.Close()
	if c.deliver(msgEvent(peerPN, peerPN, false, "A2", 1700000200, text("fails"))) {
		t.Fatal("handler must return false when the store commit fails")
	}
}

func TestUnorderedAndIgnoredPayloads(t *testing.T) {
	h, c := startPaired(t, false)
	for _, m := range []*waE2E.Message{
		{},
		{SenderKeyDistributionMessage: &waE2E.SenderKeyDistributionMessage{}},
		{ProtocolMessage: &waE2E.ProtocolMessage{Type: waE2E.ProtocolMessage_HISTORY_SYNC_NOTIFICATION.Enum()}},
	} {
		if !c.deliver(msgEvent(peerPN, peerPN, false, "X", 1700000100, m)) {
			t.Fatal("ignored payloads must be acked")
		}
	}
	if !c.deliver(&events.UndecryptableMessage{}) {
		t.Fatal("undecryptable must be acked")
	}
	if got := h.threads(); len(got) != 0 {
		t.Fatalf("threads %v", got)
	}
}

func webMsg(chat, id string, ts uint64, fromMe bool, body string) *waHistorySync.HistorySyncMsg {
	return &waHistorySync.HistorySyncMsg{Message: &waWeb.WebMessageInfo{
		Key:              &waCommon.MessageKey{RemoteJID: ptr(chat), FromMe: ptr(fromMe), ID: ptr(id)},
		Message:          text(body),
		MessageTimestamp: ptr(ts),
	}}
}

func historyEvent(t waHistorySync.HistorySync_HistorySyncType, session string, convs ...*waHistorySync.Conversation) *events.HistorySync {
	ev := &events.HistorySync{Data: &waHistorySync.HistorySync{SyncType: t.Enum(), Conversations: convs}}
	if session != "" {
		ev.Notification = &waE2E.HistorySyncNotification{PeerDataRequestSessionID: ptr(session)}
	}
	return ev
}

func TestHistorySyncBatches(t *testing.T) {
	h, c := startPaired(t, false)
	conv := &waHistorySync.Conversation{
		ID: ptr(peerPN), Name: ptr("Alice"), UnreadCount: ptr(uint32(2)),
		EndOfHistoryTransferType: waHistorySync.Conversation_COMPLETE_BUT_MORE_MESSAGES_REMAIN_ON_PRIMARY.Enum(),
	}
	for i := 0; i < 450; i++ {
		conv.Messages = append(conv.Messages, webMsg(peerPN, fmt.Sprintf("H%03d", i), uint64(1700000000+i), i%2 == 0, fmt.Sprintf("m%d", i)))
	}
	conv.Messages[3].Message.Reactions = []*waWeb.Reaction{{Key: &waCommon.MessageKey{FromMe: ptr(false)}, Text: ptr("😀")}}
	if !c.deliver(historyEvent(waHistorySync.HistorySync_INITIAL_BOOTSTRAP, "", conv)) {
		t.Fatal("history sync refused")
	}
	var sizes []int
	var completes []bool
	for i := 0; i < 3; i++ {
		ev := h.event("history")
		msgs := ev["messages"].([]any)
		sizes = append(sizes, len(msgs))
		completes = append(completes, ev["complete"].(bool))
		if ev["thread_id"] != peerPN {
			t.Fatalf("history thread %v", ev["thread_id"])
		}
		if i == 0 && msgs[0].(map[string]any)["id"] != "H449" {
			t.Fatalf("history must be newest first, got %v", msgs[0])
		}
	}
	if fmt.Sprint(sizes) != "[200 200 50]" || fmt.Sprint(completes) != "[false false true]" {
		t.Fatalf("batches %v complete %v", sizes, completes)
	}
	th := h.event("thread")["thread"].(map[string]any)
	if th["name"] != "Alice" || th["unread"] != true {
		t.Fatalf("thread %v", th)
	}
	all := h.threads()
	if len(all) != 1 || all[0]["last_message_ts"] != float64(1700000449) {
		t.Fatalf("threads %v", all)
	}
	msgs, more := h.fetch(peerPN, 100)
	if len(msgs) != 100 || !more {
		t.Fatalf("fetch %d more=%v", len(msgs), more)
	}
	chat, _, _ := h.srv.store.GetChat(context.Background(), peerPN)
	if chat.EndOfHistory != "COMPLETE_BUT_MORE_MESSAGES_REMAIN_ON_PRIMARY" || chat.UnreadCount != 2 {
		t.Fatalf("chat %+v", chat)
	}
	m, _, _ := h.srv.store.GetMessage(context.Background(), peerPN, "H003")
	if len(m.Reactions) != 1 || m.Reactions[0].Emoji != "😀" {
		t.Fatalf("history reaction not stored: %+v", m.Reactions)
	}
}

func TestHistorySyncExactBatchAndEmptyAndFiltered(t *testing.T) {
	h, c := startPaired(t, false)
	conv := &waHistorySync.Conversation{ID: ptr(peerPN)}
	for i := 0; i < 200; i++ {
		conv.Messages = append(conv.Messages, webMsg(peerPN, fmt.Sprintf("H%03d", i), uint64(1700000000+i), false, "x"))
	}
	empty := &waHistorySync.Conversation{ID: ptr(group), Name: ptr("Empty group")}
	status := &waHistorySync.Conversation{ID: ptr("status@broadcast"), Messages: []*waHistorySync.HistorySyncMsg{webMsg("status@broadcast", "S", 1, false, "s")}}
	if !c.deliver(historyEvent(waHistorySync.HistorySync_RECENT, "", conv, empty, status)) {
		t.Fatal("refused")
	}
	ev := h.event("history")
	if len(ev["messages"].([]any)) != 200 || ev["complete"] != true {
		t.Fatalf("single batch %v", ev["complete"])
	}
	if got := h.pending("history"); len(got) != 0 {
		t.Fatalf("extra history events %d", len(got))
	}
	names := map[string]bool{}
	for _, th := range h.threads() {
		names[th["id"].(string)] = true
	}
	if !names[peerPN] || !names[group] || names["status@broadcast"] || len(names) != 2 {
		t.Fatalf("threads %v", names)
	}
}

func TestHistorySyncLearnsNamesAndLIDMappings(t *testing.T) {
	h, c := startPaired(t, false)
	ev := historyEvent(waHistorySync.HistorySync_PUSH_NAME, "")
	ev.Data.Pushnames = []*waHistorySync.Pushname{{ID: ptr(peerPN), Pushname: ptr("Zed")}}
	ev.Data.PhoneNumberToLidMappings = []*waHistorySync.PhoneNumberToLIDMapping{{PnJID: ptr(peerPN), LidJID: ptr(peerLID)}}
	if !c.deliver(ev) {
		t.Fatal("refused")
	}
	evt := msgEvent(peerLID, peerLID, false, "A1", 1700000100, text("hi"))
	evt.Info.PushName = ""
	c.deliver(evt)
	th := h.event("thread")["thread"].(map[string]any)
	if th["id"] != peerPN || th["name"] != "Zed" {
		t.Fatalf("thread %v", th)
	}
}

func TestHistorySyncPersistsConversationAndStoreFailure(t *testing.T) {
	h, c := startPaired(t, false)
	_ = h.srv.store.Close()
	conv := &waHistorySync.Conversation{ID: ptr(peerPN), Messages: []*waHistorySync.HistorySyncMsg{webMsg(peerPN, "H1", 1700000000, false, "x")}}
	if c.deliver(historyEvent(waHistorySync.HistorySync_RECENT, "", conv)) {
		t.Fatal("history handler must return false on a store failure")
	}
}

func TestGroupMessageFetchesGroupInfoInBackground(t *testing.T) {
	h, c := startPaired(t, false, withTiming(func(*timing) {}))
	c.groupInfo = &types.GroupInfo{
		JID:       mustJID(group),
		GroupName: types.GroupName{Name: "Team"},
		Participants: []types.GroupParticipant{
			{JID: mustJID(peerLID), PhoneNumber: mustJID(peerPN)},
			{JID: mustJID(selfPN), LID: mustJID(selfLID)},
		},
	}
	evt := msgEvent(group, peerLID, false, "G1", 1700000100, text("hello group"))
	evt.Info.SenderAlt = mustJID(peerPN)
	if !c.deliver(evt) {
		t.Fatal("refused")
	}
	first := h.event("thread")["thread"].(map[string]any)
	if first["id"] != group || first["kind"] != "group" {
		t.Fatalf("first thread %v", first)
	}
	m := h.event("message")["message"].(map[string]any)
	if m["sender"].(map[string]any)["id"] != peerPN {
		t.Fatalf("group sender %v", m["sender"])
	}
	if got := <-c.groupCall; got.String() != group {
		t.Fatalf("GetGroupInfo for %v", got)
	}
	th := h.event("thread")["thread"].(map[string]any)
	if th["name"] != "Team" {
		t.Fatalf("thread after group info %v", th)
	}
	parts := th["participants"].([]any)
	if len(parts) != 2 || parts[0].(map[string]any)["id"] != peerPN || parts[1].(map[string]any)["is_me"] != true {
		t.Fatalf("participants %v", parts)
	}
	c.deliver(msgEvent(group, peerPN, false, "G2", 1700000200, text("again")))
	h.sync()
	select {
	case <-c.groupCall:
		t.Fatal("group info must not be refetched within the TTL")
	case <-time.After(50 * time.Millisecond):
	}
}

func TestAppStateEvents(t *testing.T) {
	h, c := startPaired(t, false)
	h.incoming(c, peerPN, "A1", 1700000100, "hello")
	h.consume("thread")
	h.consume("message")
	st := h.srv.store
	chat := func() (v struct {
		Pinned, Archived bool
		Muted            int64
		Unread           int
	}) {
		c, _, _ := st.GetChat(context.Background(), peerPN)
		v.Pinned, v.Archived, v.Muted, v.Unread = c.Pinned, c.Archived, c.MutedUntil, c.UnreadCount
		return
	}
	c.deliver(&events.Pin{JID: mustJID(peerPN), Action: &waSyncAction.PinAction{Pinned: ptr(true)}})
	c.deliver(&events.Archive{JID: mustJID(peerPN), Action: &waSyncAction.ArchiveChatAction{Archived: ptr(true)}})
	c.deliver(&events.Mute{JID: mustJID(peerPN), Action: &waSyncAction.MuteAction{Muted: ptr(true), MuteEndTimestamp: ptr(int64(1800000000))}})
	if v := chat(); !v.Pinned || !v.Archived || v.Muted != 1800000000 {
		t.Fatalf("chat flags %+v", v)
	}
	c.deliver(&events.Mute{JID: mustJID(peerPN), Action: &waSyncAction.MuteAction{Muted: ptr(false)}})
	if chat().Muted != 0 {
		t.Fatal("unmute")
	}
	if got := h.pending("thread"); len(got) != 0 {
		t.Fatalf("flag changes are not visible in the protocol: %v", got)
	}

	c.deliver(&events.MarkChatAsRead{JID: mustJID(peerPN), Action: &waSyncAction.MarkChatAsReadAction{Read: ptr(true)}})
	if th := h.event("thread")["thread"].(map[string]any); th["unread"] != false {
		t.Fatalf("read thread %v", th)
	}
	c.deliver(&events.MarkChatAsRead{JID: mustJID(peerPN), Action: &waSyncAction.MarkChatAsReadAction{Read: ptr(false)}})
	if th := h.event("thread")["thread"].(map[string]any); th["unread"] != true {
		t.Fatalf("unread thread %v", th)
	}

	c.deliver(&events.Contact{JID: mustJID(peerPN), Action: &waSyncAction.ContactAction{FullName: ptr("Alice Liddell"), FirstName: ptr("Alice")}})
	if th := h.event("thread")["thread"].(map[string]any); th["name"] != "Alice Liddell" {
		t.Fatalf("contact name %v", th)
	}
	c.deliver(&events.PushName{JID: mustJID(peerPN), NewPushName: "Ally"})
	if got := h.pending("thread"); len(got) != 0 {
		t.Fatalf("a push name must not override an address-book name: %v", got)
	}
	c.deliver(&events.DeleteChat{JID: mustJID(peerPN), Action: &waSyncAction.DeleteChatAction{}})
	if all := h.threads(); len(all) != 0 {
		t.Fatalf("chat not deleted: %v", all)
	}
}

func TestGroupAppStateEvents(t *testing.T) {
	h, c := startPaired(t, false)
	info := types.GroupInfo{JID: mustJID(group), GroupName: types.GroupName{Name: "Fresh"}, Participants: []types.GroupParticipant{{JID: mustJID(peerPN)}}}
	if !c.deliver(&events.JoinedGroup{GroupInfo: info}) {
		t.Fatal("refused")
	}
	th := h.event("thread")["thread"].(map[string]any)
	if th["id"] != group || th["name"] != "Fresh" || th["kind"] != "group" || len(th["participants"].([]any)) != 1 {
		t.Fatalf("joined thread %v", th)
	}
	c.deliver(&events.GroupInfo{JID: mustJID(group), Name: &types.GroupName{Name: "Renamed"}})
	if th := h.event("thread")["thread"].(map[string]any); th["name"] != "Renamed" {
		t.Fatalf("renamed thread %v", th)
	}
	c.deliver(&events.Pin{JID: mustJID("999@s.whatsapp.net"), Action: &waSyncAction.PinAction{Pinned: ptr(true)}})
	if got := h.pending("thread"); len(got) != 0 {
		t.Fatalf("unknown chats are ignored: %v", got)
	}
}

func TestUnreadCountsAndKinds(t *testing.T) {
	h, c := startPaired(t, false)
	h.incoming(c, peerPN, "A1", 1700000100, "one")
	h.incoming(c, peerPN, "A2", 1700000200, "two")
	chat, _, _ := h.srv.store.GetChat(context.Background(), peerPN)
	if chat.UnreadCount != 2 || chat.Kind != api.KindDM {
		t.Fatalf("chat %+v", chat)
	}
	h.incoming(c, peerPN, "A2", 1700000200, "two")
	if chat, _, _ = h.srv.store.GetChat(context.Background(), peerPN); chat.UnreadCount != 2 {
		t.Fatalf("a redelivery must not double count: %+v", chat)
	}
}

const (
	aliceGrpPN = "14155550111@s.whatsapp.net"
	aliceGrpLD = "100000000000111@lid"
	malloryPN  = "14155550666@s.whatsapp.net"
)

func groupMsg(sender string, id string, ts int64, m *waE2E.Message) *events.Message {
	e := msgEvent(group, sender, false, id, ts, m)
	e.Info.PushName = "someone"
	return e
}

// storedMsg finds a message by id in the newest 100 of a thread.
func (h *harness) storedMsg(chat, id string) (map[string]any, bool) {
	h.t.Helper()
	rows, _ := h.fetch(chat, 100)
	for _, r := range rows {
		if r["id"] == id {
			return r, true
		}
	}
	return nil, false
}

func revokeMsg(chatID, targetID string, fromMe bool, participant string) *waE2E.Message {
	key := &waCommon.MessageKey{ID: ptr(targetID), FromMe: ptr(fromMe), RemoteJID: ptr(chatID)}
	if participant != "" {
		key.Participant = ptr(participant)
	}
	return &waE2E.Message{ProtocolMessage: &waE2E.ProtocolMessage{Type: waE2E.ProtocolMessage_REVOKE.Enum(), Key: key}}
}

func editMsg(chatID, targetID string, participant, newText string) *waE2E.Message {
	key := &waCommon.MessageKey{ID: ptr(targetID), FromMe: ptr(false), RemoteJID: ptr(chatID)}
	if participant != "" {
		key.Participant = ptr(participant)
	}
	return &waE2E.Message{ProtocolMessage: &waE2E.ProtocolMessage{
		Type:          waE2E.ProtocolMessage_MESSAGE_EDIT.Enum(),
		Key:           key,
		EditedMessage: &waE2E.Message{Conversation: ptr(newText)},
	}}
}

// Review W2: a second sender reusing a stored message id must not replace the first message.
func TestReviewMessageIDCollisionOverwritesOtherSendersMessage(t *testing.T) {
	h, c := startPaired(t, false)
	if !c.deliver(groupMsg(aliceGrpPN, "COLLIDE1", 1700000100, text("Alice: the meeting is at 5"))) {
		t.Fatal("refused")
	}
	before, ok := h.storedMsg(group, "COLLIDE1")
	if !ok {
		t.Fatal("alice message missing")
	}
	h.consume("thread")
	h.consume("message")
	if !c.deliver(groupMsg(malloryPN, "COLLIDE1", 1700000200, text("Mallory: wire the money to account X"))) {
		t.Fatal("refused")
	}
	after, ok := h.storedMsg(group, "COLLIDE1")
	if !ok {
		t.Fatal("message vanished")
	}
	if strings.Contains(after["text"].(string), "Mallory") || after["sender"].(map[string]any)["id"] != before["sender"].(map[string]any)["id"] {
		t.Errorf("Alice's stored message was replaced by Mallory's (same chat, same id): %v", after)
	}
	if rows, _ := h.fetch(group, 100); len(rows) != 1 {
		t.Errorf("the forged duplicate must not be stored as a second row: %d rows", len(rows))
	}
	if got := h.pending("message"); len(got) != 0 {
		t.Errorf("an ignored message must not be reported: %v", got)
	}
	if chat, _, _ := h.srv.store.GetChat(context.Background(), group); chat.UnreadCount != 1 || chat.LastPreview != "Alice: the meeting is at 5" {
		t.Errorf("an ignored message must not touch the chat: %+v", chat)
	}
}

// Review W2: in a DM the peer reusing the id of the owner's own message must not rewrite it.
func TestReviewPeerReusingOwnMessageIDRewritesOwnerMessage(t *testing.T) {
	h, c := startPaired(t, true, fastTiming)
	h.incoming(c, peerPN, "P0", 1700000050, "hi") // creates the chat
	res := h.mustCall("send_text", map[string]any{"thread_id": peerPN, "text": "I will pay you 10 dollars"})
	own := res["message_id"].(string)
	before, _ := h.storedMsg(peerPN, own)
	if !c.deliver(msgEvent(peerPN, peerPN, false, own, 1700000600, text("I will pay you 10000 dollars"))) {
		t.Fatal("refused")
	}
	after, _ := h.storedMsg(peerPN, own)
	if after["text"] != before["text"] || after["sender"].(map[string]any)["is_me"] != true {
		t.Errorf("the owner's own sent message row was overwritten by the peer's message with the same id: %v", after)
	}
}

func TestRedeliveryWithDifferentContentIsIgnored(t *testing.T) {
	h, c := startPaired(t, false)
	h.incoming(c, peerPN, "A1", 1700000100, "original")
	h.consume("thread")
	h.consume("message")
	if !c.deliver(msgEvent(peerPN, peerPN, false, "A1", 1700000900, text("silently rewritten"))) {
		t.Fatal("refused")
	}
	m := h.event("message")["message"].(map[string]any)
	if m["text"] != "original" || m["edited"] != false || m["ts"] != float64(1700000100) {
		t.Fatalf("a redelivery reports what is stored: %v", m)
	}
	stored, _ := h.storedMsg(peerPN, "A1")
	if stored["text"] != "original" {
		t.Fatalf("stored %v", stored)
	}
	// the same message in the other id form of its sender is still a redelivery of that sender
	evt := msgEvent(peerLID, peerLID, false, "A1", 1700000950, text("rewritten via the LID form"))
	evt.Info.SenderAlt = mustJID(peerPN)
	if !c.deliver(evt) {
		t.Fatal("refused")
	}
	if stored, _ = h.storedMsg(peerPN, "A1"); stored["text"] != "original" {
		t.Fatalf("stored after the LID redelivery %v", stored)
	}
}

// Review W3: only the author can edit a stored message, and only the author or a known admin can unsend it.
func TestReviewForgedEditAndRevokeByOtherParticipant(t *testing.T) {
	h, c := startPaired(t, false)
	if !c.deliver(groupMsg(aliceGrpPN, "AL1", 1700000100, text("Alice says: it is safe"))) {
		t.Fatal("refused")
	}
	h.consume("thread")
	h.consume("message")
	if !c.deliver(groupMsg(malloryPN, "EDIT1", 1700000200, editMsg(group, "AL1", aliceGrpPN, "Alice says: send me your password"))) {
		t.Fatal("refused")
	}
	m, _ := h.storedMsg(group, "AL1")
	if strings.Contains(m["text"].(string), "password") || m["edited"] == true {
		t.Errorf("Mallory edited Alice's message: %v", m)
	}
	if !c.deliver(groupMsg(malloryPN, "REV1", 1700000300, revokeMsg(group, "AL1", false, aliceGrpPN))) {
		t.Fatal("refused")
	}
	m, _ = h.storedMsg(group, "AL1")
	if m["unsent"] == true {
		t.Errorf("Mallory unsent Alice's message: %v", m)
	}
	if got := h.pending("message"); len(got) != 0 {
		t.Errorf("ignored changes must not be reported: %v", got)
	}
	if !c.deliver(groupMsg(aliceGrpPN, "EDIT2", 1700000400, editMsg(group, "AL1", aliceGrpPN, "Alice says: it is safe (typo fixed)"))) {
		t.Fatal("refused")
	}
	if m = h.event("message")["message"].(map[string]any); m["text"] != "Alice says: it is safe (typo fixed)" || m["edited"] != true {
		t.Errorf("the author's own edit must apply: %v", m)
	}
	if !c.deliver(groupMsg(aliceGrpPN, "REV2", 1700000500, revokeMsg(group, "AL1", false, aliceGrpPN))) {
		t.Fatal("refused")
	}
	if m = h.event("message")["message"].(map[string]any); m["unsent"] != true {
		t.Errorf("the author's own revoke must apply: %v", m)
	}
}

// Review W3: in a DM the peer must not unsend or edit the owner's message.
func TestReviewPeerRevokesOwnerMessage(t *testing.T) {
	h, c := startPaired(t, true, fastTiming)
	h.incoming(c, peerPN, "P0", 1700000050, "hi")
	res := h.mustCall("send_text", map[string]any{"thread_id": peerPN, "text": "my promise"})
	own := res["message_id"].(string)
	if !c.deliver(msgEvent(peerPN, peerPN, false, "R2", 1700000700, revokeMsg(peerPN, own, false, ""))) {
		t.Fatal("refused")
	}
	if !c.deliver(msgEvent(peerPN, peerPN, false, "E2", 1700000710, editMsg(peerPN, own, "", "my promise is void"))) {
		t.Fatal("refused")
	}
	m, _ := h.storedMsg(peerPN, own)
	if m["unsent"] == true || m["text"] != "my promise" || m["edited"] == true {
		t.Errorf("peer changed the owner's sent message in the local store: %v", m)
	}
}

func TestOwnEditAndRevokeFromAnotherDeviceStillApply(t *testing.T) {
	h, c := startPaired(t, true, fastTiming)
	h.incoming(c, peerPN, "P0", 1700000050, "hi")
	own := h.mustCall("send_text", map[string]any{"thread_id": peerPN, "text": "first draft"})["message_id"].(string)
	h.consume("thread")
	h.consume("message")
	if !c.deliver(msgEvent(peerPN, selfLID, true, "OE1", 1700000600, editMsg(peerPN, own, "", "second draft"))) {
		t.Fatal("refused")
	}
	if m := h.event("message")["message"].(map[string]any); m["text"] != "second draft" || m["edited"] != true {
		t.Fatalf("owner edit %v", m)
	}
	if !c.deliver(msgEvent(peerPN, selfLID, true, "OR1", 1700000700, revokeMsg(peerPN, own, true, ""))) {
		t.Fatal("refused")
	}
	if m := h.event("message")["message"].(map[string]any); m["unsent"] != true {
		t.Fatalf("owner revoke %v", m)
	}
}

func TestEditFromTheSamePersonInAnotherIDFormApplies(t *testing.T) {
	h, c := startPaired(t, false)
	if !c.deliver(groupMsg(aliceGrpLD, "L1", 1700000100, text("from a LID"))) {
		t.Fatal("refused")
	}
	h.consume("thread")
	h.consume("message")
	evt := groupMsg(aliceGrpLD, "E1", 1700000200, editMsg(group, "L1", "", "from a LID, edited"))
	evt.Info.SenderAlt = mustJID(aliceGrpPN)
	if !c.deliver(evt) {
		t.Fatal("refused")
	}
	if m := h.event("message")["message"].(map[string]any); m["text"] != "from a LID, edited" {
		t.Fatalf("edit %v", m)
	}
}

func joinGroup(t *testing.T, h *harness, c *fakeClient, admins ...string) {
	t.Helper()
	parts := []types.GroupParticipant{{JID: mustJID(selfPN)}}
	for _, p := range []string{aliceGrpPN, malloryPN} {
		gp := types.GroupParticipant{JID: mustJID(p)}
		for _, a := range admins {
			gp.IsAdmin = gp.IsAdmin || a == p
		}
		parts = append(parts, gp)
	}
	if !c.deliver(&events.JoinedGroup{GroupInfo: types.GroupInfo{JID: mustJID(group), GroupName: types.GroupName{Name: "Crew"}, Participants: parts}}) {
		t.Fatal("refused")
	}
	h.consume("thread")
}

func TestGroupAdminMayRevokeButNotEdit(t *testing.T) {
	h, c := startPaired(t, false)
	joinGroup(t, h, c, aliceGrpPN)
	if chat, _, _ := h.srv.store.GetChat(context.Background(), group); !chat.HasAdmin(aliceGrpPN) || chat.HasAdmin(malloryPN) {
		t.Fatalf("admins %v", chat.Admins)
	}
	c.deliver(groupMsg(malloryPN, "M1", 1700000100, text("spam")))
	c.deliver(groupMsg(aliceGrpPN, "A1", 1700000110, text("announcement")))
	h.consume("message")

	c.deliver(groupMsg(aliceGrpPN, "AE", 1700000200, editMsg(group, "M1", malloryPN, "an admin may not edit")))
	if m, _ := h.storedMsg(group, "M1"); m["text"] != "spam" || m["edited"] == true {
		t.Fatalf("an admin edited another member's message: %v", m)
	}
	c.deliver(groupMsg(malloryPN, "MR", 1700000210, revokeMsg(group, "A1", false, aliceGrpPN)))
	if m, _ := h.storedMsg(group, "A1"); m["unsent"] == true {
		t.Fatalf("a plain member unsent an admin's message: %v", m)
	}
	if got := h.pending("message"); len(got) != 0 {
		t.Fatalf("ignored changes were reported: %v", got)
	}

	c.deliver(groupMsg(aliceGrpPN, "AR", 1700000300, revokeMsg(group, "M1", false, malloryPN)))
	if m := h.event("message")["message"].(map[string]any); m["id"] != "M1" || m["unsent"] != true {
		t.Fatalf("admin revoke %v", m)
	}

	// promotion and demotion events change who may unsend, without a group info fetch
	c.deliver(&events.GroupInfo{JID: mustJID(group), Promote: []types.JID{mustJID(malloryPN)}})
	c.deliver(groupMsg(malloryPN, "MR2", 1700000400, revokeMsg(group, "A1", false, aliceGrpPN)))
	if m := h.event("message")["message"].(map[string]any); m["id"] != "A1" || m["unsent"] != true {
		t.Fatalf("revoke by a promoted member %v", m)
	}
	c.deliver(&events.GroupInfo{JID: mustJID(group), Demote: []types.JID{mustJID(malloryPN)}})
	c.deliver(groupMsg(aliceGrpPN, "A2", 1700000500, text("second announcement")))
	h.consume("message")
	c.deliver(groupMsg(malloryPN, "MR3", 1700000600, revokeMsg(group, "A2", false, aliceGrpPN)))
	if m, _ := h.storedMsg(group, "A2"); m["unsent"] == true {
		t.Fatalf("revoke by a demoted member %v", m)
	}
}

func TestUnknownGroupAdminsMeanNoAdminRevoke(t *testing.T) {
	h, c := startPaired(t, false)
	c.deliver(groupMsg(malloryPN, "M1", 1700000100, text("hello")))
	h.consume("thread")
	h.consume("message")
	c.deliver(groupMsg(aliceGrpPN, "AR", 1700000200, revokeMsg(group, "M1", false, malloryPN)))
	if m, _ := h.storedMsg(group, "M1"); m["unsent"] == true {
		t.Fatalf("a revoke from a sender not known to be an admin was applied: %v", m)
	}
}

func TestGroupInfoFetchRecordsAdmins(t *testing.T) {
	h, c := startPaired(t, false, withTiming(func(*timing) {}))
	c.groupInfo = &types.GroupInfo{
		JID: mustJID(group), GroupName: types.GroupName{Name: "Team"},
		Participants: []types.GroupParticipant{
			{JID: mustJID(aliceGrpLD), PhoneNumber: mustJID(aliceGrpPN), IsSuperAdmin: true},
			{JID: mustJID(malloryPN)},
		},
	}
	c.deliver(groupMsg(malloryPN, "M1", 1700000100, text("hello")))
	<-c.groupCall
	eventually(t, "admins stored", func() bool {
		chat, _, _ := h.srv.store.GetChat(context.Background(), group)
		return chat.HasAdmin(aliceGrpPN) && chat.HasAdmin(aliceGrpLD) && !chat.HasAdmin(malloryPN)
	})
}

func TestHistoryRevokeFromAnotherSenderIsIgnored(t *testing.T) {
	h, c := startPaired(t, false)
	revoke := func(id, by string) *waHistorySync.HistorySyncMsg {
		return &waHistorySync.HistorySyncMsg{Message: &waWeb.WebMessageInfo{
			Key:              &waCommon.MessageKey{RemoteJID: ptr(group), FromMe: ptr(false), ID: ptr(id), Participant: ptr(by)},
			Message:          revokeMsg(group, "H1", false, aliceGrpPN),
			MessageTimestamp: ptr(uint64(1700000300)),
		}}
	}
	orig := webMsg(group, "H1", 1700000100, false, "kept")
	orig.Message.Key.Participant = ptr(aliceGrpPN)
	conv := &waHistorySync.Conversation{ID: ptr(group), Messages: []*waHistorySync.HistorySyncMsg{orig, revoke("R1", malloryPN)}}
	if !c.deliver(historyEvent(waHistorySync.HistorySync_RECENT, "", conv)) {
		t.Fatal("refused")
	}
	if m, _ := h.storedMsg(group, "H1"); m["unsent"] == true || m["text"] != "kept" {
		t.Fatalf("a history revoke from another sender was applied: %v", m)
	}
	conv = &waHistorySync.Conversation{ID: ptr(group), Messages: []*waHistorySync.HistorySyncMsg{revoke("R2", aliceGrpPN)}}
	if !c.deliver(historyEvent(waHistorySync.HistorySync_RECENT, "", conv)) {
		t.Fatal("refused")
	}
	if m, _ := h.storedMsg(group, "H1"); m["unsent"] != true {
		t.Fatalf("the author's history revoke must apply: %v", m)
	}
}

func TestHistoryCopyFromAnotherSenderIsIgnored(t *testing.T) {
	h, c := startPaired(t, false)
	c.deliver(groupMsg(aliceGrpPN, "G1", 1700000100, text("Alice: real")))
	h.consume("thread")
	h.consume("message")
	forged := webMsg(group, "G1", 1700000150, false, "Mallory: forged")
	forged.Message.Key.Participant = ptr(malloryPN)
	forged.Message.Reactions = []*waWeb.Reaction{{Key: &waCommon.MessageKey{FromMe: ptr(false), Participant: ptr(malloryPN)}, Text: ptr("💣")}}
	conv := &waHistorySync.Conversation{ID: ptr(group), Messages: []*waHistorySync.HistorySyncMsg{forged}}
	if !c.deliver(historyEvent(waHistorySync.HistorySync_RECENT, "", conv)) {
		t.Fatal("refused")
	}
	m, _, _ := h.srv.store.GetMessage(context.Background(), group, "G1")
	if m.Text == nil || *m.Text != "Alice: real" || m.Sender != aliceGrpPN || len(m.Reactions) != 0 {
		t.Fatalf("the forged history copy changed the stored message: %+v", m)
	}
	for _, ev := range h.pending("history") {
		if len(ev["messages"].([]any)) != 0 {
			t.Fatalf("the forged copy was reported: %v", ev)
		}
	}
}

func TestEncryptedEditFromAnotherSenderIsIgnored(t *testing.T) {
	h, c := startPaired(t, false)
	c.deliver(groupMsg(aliceGrpPN, "A1", 1700000100, text("original")))
	h.consume("thread")
	h.consume("message")
	c.decSecret = func(*events.Message) (*waE2E.Message, error) { return text("rewritten"), nil }
	secret := &waE2E.Message{SecretEncryptedMessage: &waE2E.SecretEncryptedMessage{TargetMessageKey: &waCommon.MessageKey{ID: ptr("A1")}}}
	c.deliver(groupMsg(malloryPN, "S1", 1700000200, secret))
	if m, _ := h.storedMsg(group, "A1"); m["text"] != "original" {
		t.Fatalf("an encrypted edit from another sender was applied: %v", m)
	}
	c.deliver(groupMsg(aliceGrpPN, "S2", 1700000300, secret))
	if m := h.event("message")["message"].(map[string]any); m["text"] != "rewritten" || m["edited"] != true {
		t.Fatalf("the author's encrypted edit must apply: %v", m)
	}
}

// Review L5: after a LID chat merges into the phone-number chat the daemon needs a thread event for the new id.
func TestReviewLIDMergeEmitsNoThreadEvent(t *testing.T) {
	h, c := startPaired(t, false)
	if !c.deliver(msgEvent(peerLID, peerLID, false, "L1", 1700000100, text("first as LID"))) {
		t.Fatal("refused")
	}
	h.consume("thread")
	h.consume("message")
	evt := msgEvent(peerLID, peerLID, false, "L2", 1700000200, text("second, now with PN known"))
	evt.Info.SenderAlt = mustJID(peerPN)
	if !c.deliver(evt) {
		t.Fatal("refused")
	}
	ths, msgs := h.pending("thread"), h.pending("message")
	if len(ths) != 1 || len(msgs) != 1 {
		t.Fatalf("want one thread and one message event, got %d and %d", len(ths), len(msgs))
	}
	th := ths[0]["thread"].(map[string]any)
	if th["id"] != peerPN || th["alt_id"] != peerLID || msgs[0]["message"].(map[string]any)["thread_id"] != peerPN {
		t.Errorf("thread id moved from the LID form to the PN form but the event says %v", th)
	}
}

func TestReadSelfReceiptMovesTheReadMark(t *testing.T) {
	h, c := startPaired(t, false)
	h.incoming(c, peerPN, "A1", 1700000100, "one")
	h.incoming(c, peerPN, "A2", 1700000100, "two")
	h.incoming(c, peerPN, "A3", 1700000200, "three")
	src := types.MessageSource{Chat: mustJID(peerPN), Sender: mustJID(selfPN), IsFromMe: true}
	c.deliver(&events.Receipt{MessageSource: src, MessageIDs: []string{"A1", "A2"}, Timestamp: time.Unix(1700000900, 0), Type: types.ReceiptTypeReadSelf})
	chat, _, _ := h.srv.store.GetChat(context.Background(), peerPN)
	if chat.ReadUpToTS != 1700000100 || chat.ReadUpToID != "A2" || chat.UnreadCount != 0 {
		t.Fatalf("read mark after a read on another device: %+v", chat)
	}
	h.incoming(c, peerPN, "A9", 1700000100, "arrives later in the same second")
	if chat, _, _ = h.srv.store.GetChat(context.Background(), peerPN); chat.UnreadCount != 1 {
		t.Fatalf("a later message in the same second with a larger id is unread: %+v", chat)
	}
	h.incoming(c, peerPN, "A4", 1700000300, "newer")
	if chat, _, _ = h.srv.store.GetChat(context.Background(), peerPN); chat.UnreadCount != 2 {
		t.Fatalf("unread %d", chat.UnreadCount)
	}
}

func TestIgnoredChangeIsLoggedWithIDsOnly(t *testing.T) {
	f, c := pairedFactory(false)
	h, logs := startServerLogged(t, f)
	h.mustHello()
	c.deliver(groupMsg(aliceGrpPN, "AL1", 1700000100, text("Alice says: it is safe")))
	c.deliver(groupMsg(malloryPN, "E1", 1700000200, editMsg(group, "AL1", aliceGrpPN, "forged SECRETTEXT")))
	c.deliver(groupMsg(malloryPN, "R1", 1700000300, revokeMsg(group, "AL1", false, aliceGrpPN)))
	c.deliver(groupMsg(malloryPN, "AL1", 1700000400, text("duplicate id SECRETTEXT")))
	h.sync()
	out := logs.String()
	for _, want := range []string{
		`"level":"info","op":"edit","thread":"` + group + `","id":"AL1"`,
		`"level":"info","op":"revoke","thread":"` + group + `","id":"AL1"`,
		`"level":"info","thread":"` + group + `","id":"AL1"`,
	} {
		if !strings.Contains(out, want) {
			t.Fatalf("log lacks %s:\n%s", want, out)
		}
	}
	for _, leak := range []string{"SECRETTEXT", "forged", "14155550111", "14155550666", "Alice says"} {
		if strings.Contains(out, leak) {
			t.Fatalf("the log leaks %q:\n%s", leak, out)
		}
	}
}
