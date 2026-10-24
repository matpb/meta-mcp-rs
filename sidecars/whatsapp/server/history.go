package server

import (
	"context"
	"reflect"
	"sync"
	"sync/atomic"
	"time"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/proto/waHistorySync"
	"go.mau.fi/whatsmeow/proto/waWeb"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/ipc"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/normalize"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/store"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/wa"
)

const (
	onDemandCount = 50
	endMoreRemain = "COMPLETE_BUT_MORE_MESSAGES_REMAIN_ON_PRIMARY"
	endMoreOnDem  = "COMPLETE_ON_DEMAND_SYNC_BUT_MORE_MSG_REMAIN_ON_PRIMARY"
	endNoMore     = "COMPLETE_AND_NO_MORE_MESSAGE_REMAIN_ON_PRIMARY"
)

// histWait is the one in-flight on-demand history request of a chat.
type histWait struct {
	id     string
	done   chan struct{}
	once   sync.Once
	joined atomic.Int32 // callers that piggy-backed on this request
	failed atomic.Bool  // the request could not be sent
}

func (w *histWait) finish() { w.once.Do(func() { close(w.done) }) }

// histWaiters tracks on-demand history requests by chat key.
type histWaiters struct {
	mu sync.Mutex
	m  map[string]*histWait
}

// resolve wakes the request with this session id, else the in-flight request of any listed chat.
func (h *histWaiters) resolve(sessionID string, chats []string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	for _, w := range h.m {
		if sessionID != "" && w.id == sessionID {
			w.finish()
			return
		}
	}
	for _, c := range chats {
		if w, found := h.m[c]; found {
			w.finish()
		}
	}
}

// onHistorySync stores every conversation of a history blob and reports it in batches.
func (s *server) onHistorySync(ctx context.Context, cl wa.Client, e *events.HistorySync) error {
	d := e.Data
	if d == nil {
		return nil
	}
	s.ingestMu.Lock()
	defer s.ingestMu.Unlock()
	for _, p := range d.GetPushnames() {
		if j, err := types.ParseJID(p.GetID()); err == nil {
			if err := s.store.PutPushName(ctx, j.ToNonAD().String(), p.GetPushname()); err != nil {
				return err
			}
		}
	}
	for _, m := range d.GetPhoneNumberToLidMappings() {
		pn, err1 := types.ParseJID(m.GetPnJID())
		lid, err2 := types.ParseJID(m.GetLidJID())
		if err1 != nil || err2 != nil {
			continue
		}
		if err := s.store.PutLIDMapping(ctx, lid.ToNonAD().String(), pn.ToNonAD().String()); err != nil {
			return err
		}
	}
	onDemand := d.GetSyncType() == waHistorySync.HistorySync_ON_DEMAND
	var chats []string
	for _, conv := range d.GetConversations() {
		key, err := s.ingestConversation(ctx, cl, conv, onDemand)
		if err != nil {
			return err
		}
		if key != "" {
			chats = append(chats, key)
		}
	}
	if onDemand {
		s.hist.resolve(e.Notification.GetPeerDataRequestSessionID(), chats)
	}
	return nil
}

// ingestConversation stores one conversation and emits its thread and history events; key is "" when skipped.
func (s *server) ingestConversation(ctx context.Context, cl wa.Client, conv *waHistorySync.Conversation, onDemand bool) (string, error) {
	jid, err := types.ParseJID(conv.GetID())
	if err != nil || normalize.Filtered(jid) {
		return "", nil
	}
	kind := chatKind(jid)
	alt := types.EmptyJID
	if kind == api.KindDM {
		raw := conv.GetPnJID()
		if jid.Server == types.DefaultUserServer {
			raw = conv.GetLidJID()
		}
		if a, err := types.ParseJID(raw); err == nil && raw != "" {
			alt = a
		}
	}
	chatID, chatAlt, err := s.canon(ctx, cl, jid, alt)
	if err != nil {
		return "", err
	}
	chat, created, err := s.store.EnsureChat(ctx, chatID, chatAlt, kind)
	if err != nil {
		return "", err
	}
	before := chat
	chat, _, err = s.store.UpdateChat(ctx, chat.JID, func(c *store.Chat) {
		if n := conv.GetName(); n != "" {
			c.Name = n
		} else if n := conv.GetDisplayName(); n != "" && c.Name == "" {
			c.Name = n
		}
		if conv.UnreadCount != nil && !onDemand {
			c.UnreadCount = int(conv.GetUnreadCount())
		}
		if conv.Archived != nil {
			c.Archived = conv.GetArchived()
		}
		if conv.Pinned != nil {
			c.Pinned = conv.GetPinned() > 0
		}
		if conv.MuteEndTime != nil {
			c.MutedUntil = int64(conv.GetMuteEndTime())
		}
		if conv.EndOfHistoryTransferType != nil {
			c.EndOfHistory = conv.GetEndOfHistoryTransferType().String()
		}
	})
	if err != nil {
		return "", err
	}

	var msgs []store.Msg
	for _, hm := range conv.GetMessages() {
		wm := hm.GetMessage()
		if wm == nil {
			continue
		}
		m, ok, err := s.historyMessage(ctx, cl, jid, chat, wm)
		if err != nil {
			return "", err
		}
		if ok {
			msgs = append(msgs, m)
		}
	}
	if latest, found, err := s.store.GetChat(ctx, chat.JID); err != nil {
		return "", err
	} else if found {
		chat = latest
	}
	if created || !reflect.DeepEqual(before.API(), chat.API()) {
		if err := s.emit(api.EvThread, api.ThreadEvent{Thread: chat.API()}); err != nil {
			return "", err
		}
	}
	sortNewestFirst(msgs)
	out := make([]api.Message, len(msgs))
	for i, m := range msgs {
		out[i] = m.API()
	}
	batches := normalize.Batches(out, normalize.HistoryBatchMax)
	for i, b := range batches {
		ev := api.HistoryEvent{ThreadID: chat.JID, Messages: b, Complete: i == len(batches)-1}
		if err := s.emit(api.EvHistory, ev); err != nil {
			return "", err
		}
	}
	return chat.JID, nil
}

// historyMessage parses and stores one history message; ok is false when it is skipped.
func (s *server) historyMessage(ctx context.Context, cl wa.Client, chatJID types.JID, chat store.Chat, wm *waWeb.WebMessageInfo) (store.Msg, bool, error) {
	evt, err := cl.ParseWebMessage(chatJID, wm)
	if err != nil || evt == nil {
		return store.Msg{}, false, nil
	}
	cls := normalize.Classify(evt.Message)
	o, err := s.originOf(ctx, cl, evt.Info, chat.JID, chat.AltJID)
	if err != nil {
		return store.Msg{}, false, err
	}
	switch cls.Action {
	case normalize.ActMessage:
		m := s.msgFrom(o, chat.JID, cls.Content)
		m.Edited = wm.GetMessage().GetProtocolMessage().GetType() == waE2E.ProtocolMessage_MESSAGE_EDIT
		if _, err := s.store.PutMessage(ctx, m, false); err != nil {
			return m, false, err
		}
		for _, r := range wm.GetReactions() {
			sender, err := s.reactionSender(ctx, cl, o, r.GetKey().GetFromMe(), r.GetKey().GetParticipant())
			if err != nil {
				return m, false, err
			}
			ts := r.GetSenderTimestampMS() / 1000
			if ts == 0 {
				ts = m.TS
			}
			if err := s.store.PutReaction(ctx, chat.JID, m.ID, sender, r.GetText(), ts); err != nil {
				return m, false, err
			}
		}
		stored, found, err := s.store.GetMessage(ctx, chat.JID, m.ID)
		if err != nil {
			return m, false, err
		}
		if found {
			m = stored
		}
		return m, true, nil
	case normalize.ActRevoke:
		_, _, err := s.store.UpdateMessage(ctx, chat.JID, cls.TargetID, func(m *store.Msg) {
			m.Unsent, m.Text, m.Attachments, m.Raw = true, nil, []api.Attachment{}, nil
		})
		return store.Msg{}, false, err
	case normalize.ActReaction:
		err := s.store.PutReaction(ctx, chat.JID, cls.TargetID, o.senderID, cls.Emoji, o.info.Timestamp.Unix())
		return store.Msg{}, false, err
	}
	return store.Msg{}, false, nil
}

// reactionSender resolves the id of a history reaction's author.
func (s *server) reactionSender(ctx context.Context, cl wa.Client, o origin, fromMe bool, participant string) (string, error) {
	switch {
	case fromMe && !o.self.JID.IsEmpty():
		return o.self.JID.String(), nil
	case participant != "":
		if j, err := types.ParseJID(participant); err == nil {
			id, _, err := s.canon(ctx, cl, j, types.EmptyJID)
			return id, err
		}
	}
	return o.chatID, nil
}

type fetchParams struct {
	ThreadID string `json:"thread_id"`
	Limit    int    `json:"limit"`
	BeforeMS int64  `json:"before_ts_ms"`
	BeforeID string `json:"before_id"`
}

// fetchMessages serves a page from the store and asks the phone for older history when the store runs out.
func (s *server) fetchMessages(ctx context.Context, req ipc.Request) ipc.Reply {
	var p fetchParams
	if e := decode(req, &p); e != nil {
		return failErr(req.ID, e)
	}
	if p.ThreadID == "" || p.Limit < 1 || p.Limit > 100 {
		return fail(req.ID, api.CodeBadParams, "thread_id is required and limit must be 1 to 100")
	}
	chat, err := s.chatFor(ctx, p.ThreadID)
	if err != nil {
		return failErr(req.ID, err)
	}
	rows, more, err := s.store.FetchMessages(ctx, chat.JID, p.Limit, p.BeforeMS, p.BeforeID)
	if err != nil {
		return fail(req.ID, api.CodeIO, "message database error")
	}
	if len(rows) < p.Limit {
		cl := s.cur()
		if cl.HasDevice() && cl.IsConnected() && chat.EndOfHistory != endNoMore {
			if answered := s.requestHistory(ctx, cl, chat); answered {
				if rows, more, err = s.store.FetchMessages(ctx, chat.JID, p.Limit, p.BeforeMS, p.BeforeID); err != nil {
					return fail(req.ID, api.CodeIO, "message database error")
				}
			}
		}
	}
	if !more {
		if upd, found, err := s.store.GetChat(ctx, chat.JID); err == nil && found {
			more = upd.EndOfHistory == endMoreRemain || upd.EndOfHistory == endMoreOnDem
		}
	}
	msgs := make([]api.Message, len(rows))
	for i, m := range rows {
		msgs[i] = m.API()
	}
	return ok(req.ID, map[string]any{"messages": msgs, "has_more": more})
}

// requestHistory asks the phone for older messages and reports whether it answered in time.
func (s *server) requestHistory(ctx context.Context, cl wa.Client, chat store.Chat) bool {
	if s.blockedReason() != "" {
		return false
	}
	oldest, found, err := s.store.OldestMessage(ctx, chat.JID)
	if err != nil || !found {
		return false
	}
	s.hist.mu.Lock()
	w, inFlight := s.hist.m[chat.JID]
	if inFlight {
		w.joined.Add(1)
	} else {
		w = &histWait{id: string(cl.GenerateMessageID()), done: make(chan struct{})}
		s.hist.m[chat.JID] = w
	}
	s.hist.mu.Unlock()

	if !inFlight {
		defer func() {
			s.hist.mu.Lock()
			if s.hist.m[chat.JID] == w {
				delete(s.hist.m, chat.JID)
			}
			s.hist.mu.Unlock()
		}()
		if err := s.sendHistoryRequest(ctx, cl, chat, oldest, w.id); err != nil {
			w.failed.Store(true)
			w.finish()
			return false
		}
	}
	timer := time.NewTimer(s.tm.history)
	defer timer.Stop()
	select {
	case <-w.done:
		return !w.failed.Load()
	case <-timer.C:
	case <-ctx.Done():
	}
	return false
}

func (s *server) sendHistoryRequest(ctx context.Context, cl wa.Client, chat store.Chat, oldest store.Msg, id string) error {
	chatWire, err1 := types.ParseJID(oldest.ChatWire)
	sender, err2 := types.ParseJID(oldest.SenderWire)
	if err1 != nil || err2 != nil {
		return context.Canceled
	}
	anchor := &types.MessageInfo{
		MessageSource: types.MessageSource{Chat: chatWire, Sender: sender, IsFromMe: oldest.FromMe, IsGroup: chat.Kind == api.KindGroup},
		ID:            oldest.ID,
		Timestamp:     time.Unix(oldest.TS, 0),
	}
	msg := cl.BuildHistorySyncRequest(anchor, onDemandCount)
	_, err := cl.SendMessage(ctx, cl.Self().JID.ToNonAD(), msg, whatsmeow.SendRequestExtra{ID: id, Peer: true})
	return err
}
