package server

import (
	"context"
	"fmt"
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
