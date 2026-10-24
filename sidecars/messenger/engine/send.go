package engine

import (
	"context"
	"errors"
	"strconv"
	"time"
	"unicode/utf8"

	"go.mau.fi/mautrix-meta/pkg/messagix/methods"
	"go.mau.fi/mautrix-meta/pkg/messagix/socket"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
	mtypes "go.mau.fi/mautrix-meta/pkg/messagix/types"
	"go.mau.fi/mautrix-meta/pkg/metaid"
	"go.mau.fi/util/variationselector"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waCommon"
	"go.mau.fi/whatsmeow/proto/waConsumerApplication"
	"go.mau.fi/whatsmeow/proto/waMsgApplication"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	"google.golang.org/protobuf/proto"
	"maunium.net/go/mautrix/bridgev2/networkid"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
	"github.com/matpb/meta-mcp-rs/sidecars/messenger/normalize"
)

// Limits and waits; tests lower the waits.
var (
	maxTextRunes    = 20000
	canSendWait     = 15 * time.Second
	sendExecTimeout = 60 * time.Second
	sendOutcomeWait = 30 * time.Second
	e2eeSendTimeout = 30 * time.Second
)

const communitySyncGroup = 104

// threadTarget is where an action goes: the Messenger thread key or the E2EE chat JID.
type threadTarget struct {
	id    string
	entry threadEntry
	known bool
	fbKey int64
	tt    table.ThreadType
	enc   bool
	chat  types.JID
}

func (t threadTarget) isGroup() bool {
	if t.known {
		return t.entry.Kind == api.KindGroup
	}
	return !t.tt.IsOneToOne()
}

// chatJIDFor is the E2EE chat of a thread: msgr for 1:1, g.us for groups.
func chatJIDFor(e threadEntry) types.JID {
	server := e.WAServer
	if server == "" {
		server = types.MessengerServer
		if e.Kind == api.KindGroup {
			server = types.GroupServer
		}
	}
	return types.JID{User: e.ID, Server: server}
}

// usable fails when the session was closed or logged out.
func (s *session) usable() error {
	s.mu.Lock()
	out := s.loggedOut || s.wiped
	s.mu.Unlock()
	if s.closed.Load() || out {
		return api.Errorf(api.CodeNotLoggedIn, "not logged in")
	}
	return nil
}

// resolveThread routes a thread id to the encrypted or unencrypted side.
func (s *session) resolveThread(id string) (threadTarget, error) {
	key, err := strconv.ParseInt(id, 10, 64)
	if err != nil || key <= 0 {
		return threadTarget{}, api.Errorf(api.CodeThreadNotFound, "thread not found")
	}
	if e, ok := s.threadInfo(id); ok {
		t := threadTarget{id: id, entry: e, known: true, fbKey: key, tt: e.ThreadType, enc: e.encrypted()}
		if e.FBThreadKey > 0 {
			t.fbKey = e.FBThreadKey
		}
		if t.tt == table.UNKNOWN_THREAD_TYPE {
			t.tt = table.GROUP_THREAD
			if e.Kind == api.KindDM {
				t.tt = table.ONE_TO_ONE
			}
		}
		if t.enc {
			t.chat = chatJIDFor(e)
		}
		return t, nil
	}
	if s.names.get(key) != "" {
		return threadTarget{id: id, fbKey: key, tt: table.ONE_TO_ONE}, nil
	}
	return threadTarget{}, api.Errorf(api.CodeThreadNotFound, "thread not found")
}

var errWrongThread = api.Errorf(api.CodeBadParams, "message id does not belong to this thread")

// checkMessageID parses a message id and verifies it fits the thread's side.
func (s *session) checkMessageID(t threadTarget, id string) (metaid.ParsedMessageID, error) {
	parsed := metaid.ParseMessageID(networkid.MessageID(id))
	switch p := parsed.(type) {
	case metaid.ParsedFBMessageID:
		if t.enc || p.ID == "" {
			return nil, errWrongThread
		}
		if m, _, ok := s.hist.get(id); ok && m.ThreadID != t.id {
			return nil, errWrongThread
		}
		return p, nil
	case metaid.ParsedWAMessageID:
		if !t.enc || p.Chat.User != t.chat.User || p.ID == "" {
			return nil, errWrongThread
		}
		return p, nil
	}
	return nil, api.Errorf(api.CodeBadParams, "invalid message id")
}

// sendClient returns the connected Messenger client once it can send.
func (s *session) sendClient(ctx context.Context) (metaClient, error) {
	cl, err := s.liveClient()
	if err != nil {
		return nil, err
	}
	if err := cl.WaitUntilCanSendMessages(ctx, canSendWait); err != nil {
		return nil, api.Errorf(api.CodeNotConnected, "not connected")
	}
	return cl, nil
}

// liveClient returns the Messenger client when its socket is up.
func (s *session) liveClient() (metaClient, error) {
	cl := s.messagix()
	if cl == nil || !cl.IsConnected() {
		return nil, api.Errorf(api.CodeNotConnected, "not connected")
	}
	return cl, nil
}

// e2eeReady returns the connected encrypted client.
func (s *session) e2eeReady() (*whatsmeow.Client, error) {
	wa := s.e2eeClient()
	if wa == nil || !wa.IsConnected() {
		return nil, api.Errorf(api.CodeNotConnected, "not connected")
	}
	return wa, nil
}

// accountFailure applies a fail-closed verdict from a library error and returns its protocol error.
func (s *session) accountFailure(err error) *api.Error {
	f := classify(err)
	switch f.kind {
	case fatalLoggedOut:
		s.failClosed(f)
		return f.apiError()
	case fatalConn:
		return f.apiError()
	}
	return nil
}

// sendExecError maps an ExecuteTasks error after a send was written; anything unclear is send_unknown.
func (s *session) sendExecError(err error) *api.Error {
	if e := s.accountFailure(err); e != nil {
		return e
	}
	if errors.Is(err, mtypes.ErrPleaseReloadPage) {
		s.requestRecycle(false)
	}
	if errors.Is(err, whatsmeow.ErrClientIsNil) {
		return api.Errorf(api.CodeNotConnected, "not connected")
	}
	return api.Errorf(api.CodeSendUnknown, "the outcome of the send is unknown")
}

// opError maps an error of a write that has no outcome to wait for.
func (s *session) opError(err error, what string) *api.Error {
	if e := s.accountFailure(err); e != nil {
		return e
	}
	if errors.Is(err, whatsmeow.ErrClientIsNil) {
		return api.Errorf(api.CodeNotConnected, "not connected")
	}
	if errors.Is(err, mtypes.ErrPleaseReloadPage) {
		s.requestRecycle(false)
	}
	return api.Errorf(api.CodeSendFailed, "%s failed", what)
}

func validText(text string) error {
	if text == "" {
		return api.Errorf(api.CodeBadParams, "text is empty")
	}
	if utf8.RuneCountInString(text) > maxTextRunes {
		return api.Errorf(api.CodeBadParams, "text is too long")
	}
	return nil
}

// SendText sends a text message to a Messenger or encrypted thread.
func (s *session) SendText(ctx context.Context, p api.SendTextParams) (api.SendResult, error) {
	if err := s.usable(); err != nil {
		return api.SendResult{}, err
	}
	if err := validText(p.Text); err != nil {
		return api.SendResult{}, err
	}
	t, err := s.resolveThread(p.ThreadID)
	if err != nil {
		return api.SendResult{}, err
	}
	var reply metaid.ParsedMessageID
	if p.ReplyTo != "" {
		if reply, err = s.checkMessageID(t, p.ReplyTo); err != nil {
			return api.SendResult{}, err
		}
	}
	if t.enc {
		wa, err := s.e2eeReady()
		if err != nil {
			return api.SendResult{}, err
		}
		wp, _ := reply.(metaid.ParsedWAMessageID)
		var meta *waMsgApplication.MessageApplication_Metadata
		if reply != nil {
			meta = e2eeReplyMeta(wp)
		}
		return s.sendE2EE(ctx, wa, t, e2eeTextApp(p.Text), meta)
	}
	cl, err := s.sendClient(ctx)
	if err != nil {
		return api.SendResult{}, err
	}
	task := s.newSendTask(t, table.TEXT)
	task.Text = p.Text
	if fp, ok := reply.(metaid.ParsedFBMessageID); ok {
		task.ReplyMetaData = s.replyMeta(p.ReplyTo, fp)
	}
	return s.executeSend(ctx, cl, task)
}

// newSendTask fills the fields the connector sets on every SendMessageTask.
func (s *session) newSendTask(t threadTarget, sendType table.SendType) *socket.SendMessageTask {
	task := &socket.SendMessageTask{
		ThreadId:         t.fbKey,
		Otid:             methods.GenerateEpochID(),
		Source:           table.MESSENGER_INBOX_IN_THREAD,
		InitiatingSource: table.FACEBOOK_INBOX,
		SendType:         sendType,
		SyncGroup:        1,
	}
	if t.tt == table.COMMUNITY_GROUP {
		task.SyncGroup = communitySyncGroup
	}
	return task
}

func (s *session) replyMeta(id string, p metaid.ParsedFBMessageID) *socket.ReplyMetaData {
	meta := &socket.ReplyMetaData{ReplyMessageId: p.ID, ReplySourceType: 1, ReplyType: 0}
	if m, _, ok := s.hist.get(id); ok {
		meta.ReplySender, _ = strconv.ParseInt(m.Sender.ID, 10, 64)
	}
	return meta
}

// executeSend runs a send task and waits for the answer keyed by its OTID, all within one sendOutcomeWait budget.
func (s *session) executeSend(ctx context.Context, cl metaClient, task *socket.SendMessageTask) (api.SendResult, error) {
	key := strconv.FormatInt(task.Otid, 10)
	ch := s.sends.register(key)
	ectx, cancel := context.WithTimeout(ctx, sendOutcomeWait)
	defer cancel()
	tbl, err := cl.ExecuteTasks(ectx, task)
	if err != nil {
		s.sends.cancel(key)
		return api.SendResult{}, s.sendExecError(err)
	}
	untaggedFailure := false
	if tbl != nil {
		s.sends.resolve(normalize.Table(tbl, s.normContext()).Sends)
		untaggedFailure = hasUntaggedFailure(tbl)
	}
	out, ok := s.awaitOutcome(ectx, ch, untaggedFailure)
	s.sends.cancel(key)
	switch {
	case !ok:
		if untaggedFailure {
			return api.SendResult{}, api.Errorf(api.CodeSendFailed, "facebook rejected the message")
		}
		return api.SendResult{}, api.Errorf(api.CodeSendUnknown, "the outcome of the send is unknown")
	case out.Failed:
		return api.SendResult{}, api.Errorf(api.CodeSendFailed, "facebook rejected the message")
	case out.MessageID == "" || out.MessageID == "fb:":
		return api.SendResult{}, api.Errorf(api.CodeSendUnknown, "the outcome of the send is unknown")
	}
	return api.SendResult{MessageID: out.MessageID, TS: time.Now().Unix()}, nil
}

// awaitOutcome waits for the OTID's answer until ctx ends; with an untagged failure already seen it only peeks.
func (s *session) awaitOutcome(ctx context.Context, ch <-chan api.SendOutcome, peekOnly bool) (api.SendOutcome, bool) {
	if peekOnly {
		select {
		case o := <-ch:
			return o, true
		default:
			return api.SendOutcome{}, false
		}
	}
	select {
	case o := <-ch:
		return o, true
	case <-ctx.Done():
	}
	return api.SendOutcome{}, false
}

// hasUntaggedFailure reports failure rows that carry no OTID, which no waiter can match.
func hasUntaggedFailure(tbl *table.LSTable) bool {
	for _, f := range tbl.LSMarkOptimisticMessageFailed {
		if f.OTID == "" {
			return true
		}
	}
	for _, f := range tbl.LSHandleFailedTask {
		if f.OTID == "" {
			return true
		}
	}
	return false
}

// onSendOutcomes receives the server's answers to optimistic sends found in a table.
func (s *session) onSendOutcomes(outcomes []api.SendOutcome) { s.sends.resolve(outcomes) }

// e2eeTextApp builds the text ConsumerApplication the connector sends.
func e2eeTextApp(text string) *waConsumerApplication.ConsumerApplication {
	return consumerContent(&waConsumerApplication.ConsumerApplication_Content{
		Content: &waConsumerApplication.ConsumerApplication_Content_MessageText{
			MessageText: &waCommon.MessageText{Text: &text},
		},
	})
}

func consumerContent(c *waConsumerApplication.ConsumerApplication_Content) *waConsumerApplication.ConsumerApplication {
	return &waConsumerApplication.ConsumerApplication{
		Payload: &waConsumerApplication.ConsumerApplication_Payload{
			Payload: &waConsumerApplication.ConsumerApplication_Payload_Content{Content: c},
		},
	}
}

// e2eeReplyMeta quotes the replied-to message the way ToWhatsApp does.
func e2eeReplyMeta(id metaid.ParsedWAMessageID) *waMsgApplication.MessageApplication_Metadata {
	return &waMsgApplication.MessageApplication_Metadata{
		QuotedMessage: &waMsgApplication.MessageApplication_Metadata_QuotedMessage{
			StanzaID:    proto.String(id.ID),
			Participant: proto.String(id.Sender.String()),
		},
	}
}

// sendE2EE sends a ConsumerApplication and records the sent message in the encrypted store.
func (s *session) sendE2EE(ctx context.Context, wa *whatsmeow.Client, t threadTarget, app *waConsumerApplication.ConsumerApplication, meta *waMsgApplication.MessageApplication_Metadata) (api.SendResult, error) {
	sctx, cancel := context.WithTimeout(ctx, e2eeSendTimeout)
	defer cancel()
	id := wa.GenerateMessageID()
	resp, err := wa.SendFBMessage(sctx, t.chat, app, meta, whatsmeow.SendRequestExtra{ID: id})
	if err != nil {
		return api.SendResult{}, classifyE2EESend(err)
	}
	ts := resp.Timestamp
	if ts.IsZero() {
		ts = time.Now()
	}
	own := s.ownJID(wa)
	msgID := string(metaid.MakeWAMessageID(t.chat, own, id))
	s.recordSent(t, own, id, ts, app, meta)
	return api.SendResult{MessageID: msgID, TS: ts.Unix()}, nil
}

// ownJID is the account's non-device JID on the encrypted side.
func (s *session) ownJID(wa *whatsmeow.Client) types.JID {
	if wa != nil && wa.Store != nil && wa.Store.ID != nil {
		return wa.Store.ID.ToNonAD()
	}
	return types.JID{User: strconv.FormatInt(s.me.Load(), 10), Server: types.MessengerServer}
}

// recordSent stores a sent encrypted message through the same normaliser incoming ones use.
func (s *session) recordSent(t threadTarget, own types.JID, id string, ts time.Time, app *waConsumerApplication.ConsumerApplication, meta *waMsgApplication.MessageApplication_Metadata) {
	st := s.e2eeStore()
	if st == nil {
		return
	}
	evt := &events.FBMessage{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{Chat: t.chat, Sender: own, IsFromMe: true, IsGroup: t.chat.Server == types.GroupServer},
			ID:            id,
			Timestamp:     ts,
		},
		Message:       app,
		FBApplication: &waMsgApplication.MessageApplication{Metadata: meta},
	}
	res := normalize.FBMessage(evt, normalize.E2EEContext{Me: s.me.Load(), Name: s.names.get})
	if res.Message == nil {
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), e2eeOpTimeout)
	defer cancel()
	if err := st.Put(ctx, *res.Message, res.TSMS, res.Media); err != nil {
		s.log.Warn().Msg("storing a sent e2ee message failed")
		return
	}
	if _, _, changed := s.reg.noteE2EE(t.chat, s.me.Load(), res.TSMS, previewOf(*res.Message)); changed {
		s.emitThreads([]string{t.id})
	}
}

// classifyE2EESend maps a SendFBMessage error: not sent, rejected by the server, or unknown.
func classifyE2EESend(err error) *api.Error {
	switch {
	case errors.Is(err, whatsmeow.ErrNotConnected), errors.Is(err, whatsmeow.ErrNotLoggedIn), errors.Is(err, whatsmeow.ErrClientIsNil):
		return api.Errorf(api.CodeNotConnected, "not connected")
	case errors.Is(err, whatsmeow.ErrServerReturnedError), errors.Is(err, whatsmeow.ErrRecipientADJID), errors.Is(err, whatsmeow.ErrUnknownServer):
		return api.Errorf(api.CodeSendFailed, "the encrypted message was rejected")
	}
	return api.Errorf(api.CodeSendUnknown, "the outcome of the send is unknown")
}

// waKey builds the message key of a wa: message id the way the connector's messageIDToWAKey does.
func waKey(id metaid.ParsedWAMessageID, me string) *waCommon.MessageKey {
	key := &waCommon.MessageKey{RemoteJID: proto.String(id.Chat.String()), ID: proto.String(id.ID)}
	if id.Sender.User == me {
		key.FromMe = proto.Bool(true)
	}
	if id.Chat.Server != types.MessengerServer && id.Chat.Server != types.DefaultUserServer {
		key.Participant = proto.String(id.Sender.String())
	}
	return key
}

// e2eeReactionApp wraps a reaction; an empty emoji removes it.
func e2eeReactionApp(key *waCommon.MessageKey, emoji string, tsMS int64) *waConsumerApplication.ConsumerApplication {
	return consumerContent(&waConsumerApplication.ConsumerApplication_Content{
		Content: &waConsumerApplication.ConsumerApplication_Content_ReactionMessage{
			ReactionMessage: &waConsumerApplication.ConsumerApplication_ReactionMessage{
				Key:               key,
				Text:              proto.String(emoji),
				SenderTimestampMS: proto.Int64(tsMS),
			},
		},
	})
}

// SendReaction sets or removes the account's reaction on a message.
func (s *session) SendReaction(ctx context.Context, p api.SendReactionParams) error {
	if err := s.usable(); err != nil {
		return err
	}
	t, err := s.resolveThread(p.ThreadID)
	if err != nil {
		return err
	}
	parsed, err := s.checkMessageID(t, p.MessageID)
	if err != nil {
		return err
	}
	emoji := variationselector.Remove(p.Emoji)
	now := time.Now()
	me := s.me.Load()
	if wp, ok := parsed.(metaid.ParsedWAMessageID); ok {
		wa, err := s.e2eeReady()
		if err != nil {
			return err
		}
		sctx, cancel := context.WithTimeout(ctx, e2eeSendTimeout)
		defer cancel()
		app := e2eeReactionApp(waKey(wp, strconv.FormatInt(me, 10)), emoji, now.UnixMilli())
		if _, err := wa.SendFBMessage(sctx, t.chat, app, nil, whatsmeow.SendRequestExtra{ID: wa.GenerateMessageID()}); err != nil {
			if e := classifyE2EESend(err); e.Code == api.CodeNotConnected {
				return e
			}
			return api.Errorf(api.CodeSendFailed, "reaction failed")
		}
		s.recordReaction(t, p.MessageID, emoji, now)
		return nil
	}
	cl, err := s.liveClient()
	if err != nil {
		return err
	}
	fp := parsed.(metaid.ParsedFBMessageID)
	ectx, cancel := context.WithTimeout(ctx, sendExecTimeout)
	defer cancel()
	_, err = cl.ExecuteTasks(ectx, &socket.SendReactionTask{
		ThreadKey:       t.fbKey,
		TimestampMs:     now.UnixMilli(),
		MessageID:       fp.ID,
		ActorID:         me,
		Reaction:        emoji,
		SyncGroup:       1,
		SendAttribution: table.MESSENGER_INBOX_IN_THREAD,
	})
	if err != nil {
		return s.opError(err, "reaction")
	}
	return nil
}

// recordReaction mirrors our own encrypted reaction into the store, which never echoes it back.
func (s *session) recordReaction(t threadTarget, msgID, emoji string, at time.Time) {
	st := s.e2eeStore()
	if st == nil {
		return
	}
	ev := api.ReactionEvent{ThreadID: t.id, MessageID: msgID, SenderID: strconv.FormatInt(s.me.Load(), 10), TS: at.Unix()}
	if emoji != "" {
		ev.Emoji = &emoji
	}
	ctx, cancel := context.WithTimeout(context.Background(), e2eeOpTimeout)
	defer cancel()
	if _, _, err := st.ApplyReaction(ctx, ev); err != nil {
		s.log.Warn().Msg("storing a sent e2ee reaction failed")
	}
}

// MarkRead marks a thread read up to a timestamp (default now).
func (s *session) MarkRead(ctx context.Context, p api.MarkReadParams) error {
	if err := s.usable(); err != nil {
		return err
	}
	t, err := s.resolveThread(p.ThreadID)
	if err != nil {
		return err
	}
	now := time.Now()
	upTo := now.UnixMilli()
	if p.UpToTSMS != nil {
		upTo = *p.UpToTSMS
	}
	if t.enc {
		return s.markReadE2EE(ctx, t, p.UpToTSMS, now)
	}
	cl, err := s.liveClient()
	if err != nil {
		return err
	}
	ectx, cancel := context.WithTimeout(ctx, sendExecTimeout)
	defer cancel()
	if _, err := cl.ExecuteTasks(ectx, &socket.ThreadMarkReadTask{ThreadId: t.fbKey, LastReadWatermarkTs: upTo, SyncGroup: 1}); err != nil {
		return s.opError(err, "mark read")
	}
	return nil
}

const markReadWindow = 100

// markReadE2EE sends read receipts for the newest incoming stored messages up to the watermark.
func (s *session) markReadE2EE(ctx context.Context, t threadTarget, upToMS *int64, now time.Time) error {
	wa, err := s.e2eeReady()
	if err != nil {
		return err
	}
	st, err := s.ensureStore(ctx)
	if err != nil {
		return err
	}
	msgs, _, err := st.List(ctx, t.id, nil, "", markReadWindow)
	if err != nil {
		return api.AsError(err)
	}
	for key, ids := range groupUnread(msgs, upToMS) {
		if err := wa.MarkRead(ctx, ids, now, t.chat, key); err != nil {
			s.log.Warn().Int("count", len(ids)).Msg("e2ee read receipt failed")
			return api.Errorf(api.CodeIO, "read receipt failed")
		}
	}
	return nil
}

// groupUnread collects incoming wa: message ids sent at or before upToMS (or all), keyed by group sender.
func groupUnread(msgs []api.Message, upToMS *int64) map[types.JID][]types.MessageID {
	out := map[types.JID][]types.MessageID{}
	for _, m := range msgs {
		if m.Sender.IsMe || m.Unsent || (upToMS != nil && m.TS*1000 > *upToMS) {
			continue
		}
		p, ok := metaid.ParseMessageID(networkid.MessageID(m.ID)).(metaid.ParsedWAMessageID)
		if !ok {
			continue
		}
		var key types.JID
		if p.Sender != p.Chat {
			key = p.Sender
		}
		out[key] = append(out[key], p.ID)
	}
	return out
}

// SetTyping toggles the typing indicator of one thread only.
func (s *session) SetTyping(ctx context.Context, p api.SetTypingParams) error {
	if err := s.usable(); err != nil {
		return err
	}
	t, err := s.resolveThread(p.ThreadID)
	if err != nil {
		return err
	}
	if t.enc {
		wa, err := s.e2eeReady()
		if err != nil {
			return err
		}
		state := types.ChatPresencePaused
		if p.On {
			state = types.ChatPresenceComposing
		}
		if err := wa.SendChatPresence(ctx, t.chat, state, types.ChatPresenceMediaText); err != nil {
			return api.Errorf(api.CodeIO, "typing update failed")
		}
		return nil
	}
	cl, err := s.liveClient()
	if err != nil {
		return err
	}
	if err := cl.ExecuteStatelessTask(ctx, typingTask(t, p.On)); err != nil {
		return s.opError(err, "typing update")
	}
	return nil
}

func typingTask(t threadTarget, on bool) *socket.UpdatePresenceTask {
	task := &socket.UpdatePresenceTask{ThreadKey: t.fbKey, Attribution: 0, SyncGroup: 1, ThreadType: int64(t.tt)}
	if t.isGroup() {
		task.IsGroupThread = 1
	}
	if on {
		task.IsTyping = 1
	}
	return task
}
