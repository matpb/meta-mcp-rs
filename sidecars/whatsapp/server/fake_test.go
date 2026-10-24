package server

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"time"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waCommon"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/proto/waMmsRetry"
	"go.mau.fi/whatsmeow/proto/waWeb"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/wa"
)

const (
	selfPN  = "15551234567@s.whatsapp.net"
	selfLID = "99887766@lid"
)

func mustJID(s string) types.JID {
	j, err := types.ParseJID(s)
	if err != nil {
		panic(err)
	}
	return j
}

type sentMsg struct {
	to    types.JID
	msg   *waE2E.Message
	extra whatsmeow.SendRequestExtra
}

type markCall struct {
	ids    []types.MessageID
	chat   types.JID
	sender types.JID
}

type chatPresCall struct {
	jid   types.JID
	state types.ChatPresence
}

type reactionCall struct {
	chat, sender types.JID
	id           string
	emoji        string
}

// fakeClient is a scripted wa.Client that records every call and never touches the network.
type fakeClient struct {
	mu      sync.Mutex
	handler func(any) bool
	prePair func(types.JID, string, string) bool

	self      wa.Self
	hasDevice bool
	connected bool

	connectErr error
	connects   int
	disconns   int
	logouts    int
	logoutErr  error
	deletes    int

	qr          chan whatsmeow.QRChannelItem
	qrErr       error
	pairPhoneIn string
	pairCode    string
	pairErr     error
	passkeys    []*types.WebAuthnResponse
	confirms    int
	passkeyErr  error
	confirmErr  error
	prekeys     chan struct{}

	presences  chan types.Presence
	chatPres   []chatPresCall
	sent       []sentMsg
	sendFn     func(to types.JID, msg *waE2E.Message, extra whatsmeow.SendRequestExtra) (whatsmeow.SendResponse, error)
	marks      []markCall
	markErr    error
	reactions  []reactionCall
	uploads    []whatsmeow.MediaType
	uploadResp whatsmeow.UploadResponse
	uploadErr  error
	downloadFn func(msg whatsmeow.DownloadableMessage) ([]byte, error)
	downloads  []string
	receipts   chan *types.MessageInfo
	decryptRet func(evt *events.MediaRetry, key []byte) (*waMmsRetry.MediaRetryNotification, error)

	decReaction func(evt *events.Message) (*waE2E.ReactionMessage, error)
	decSecret   func(evt *events.Message) (*waE2E.Message, error)

	lidToPN   map[string]types.JID
	pnToLID   map[string]types.JID
	groupInfo *types.GroupInfo
	groupErr  error
	groupCall chan types.JID

	idCounter int
}

func newFakeClient() *fakeClient {
	return &fakeClient{
		self:      wa.Self{},
		qr:        make(chan whatsmeow.QRChannelItem, 16),
		presences: make(chan types.Presence, 8),
		receipts:  make(chan *types.MessageInfo, 8),
		groupCall: make(chan types.JID, 8),
		lidToPN:   map[string]types.JID{},
		pnToLID:   map[string]types.JID{},
	}
}

// newPairedClient returns a client that already has a linked device.
func newPairedClient(connected bool) *fakeClient {
	c := newFakeClient()
	c.pair()
	c.connected = connected
	return c
}

func (c *fakeClient) pair() {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.hasDevice = true
	c.self = wa.Self{JID: mustJID(selfPN), LID: mustJID(selfLID), PushName: "Me"}
}

// deliver passes an event to the registered handler and returns its verdict.
func (c *fakeClient) deliver(evt any) bool {
	c.mu.Lock()
	h := c.handler
	c.mu.Unlock()
	if h == nil {
		panic("no event handler registered")
	}
	return h(evt)
}

func (c *fakeClient) sentSnapshot() []sentMsg {
	c.mu.Lock()
	defer c.mu.Unlock()
	return append([]sentMsg(nil), c.sent...)
}

func (c *fakeClient) SetEventHandler(h func(any) bool) {
	c.mu.Lock()
	c.handler = h
	c.mu.Unlock()
}

func (c *fakeClient) SetPrePairCallback(cb func(types.JID, string, string) bool) {
	c.mu.Lock()
	c.prePair = cb
	c.mu.Unlock()
}

func (c *fakeClient) Connect() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.connects++
	if c.connectErr != nil {
		return c.connectErr
	}
	c.connected = true
	return nil
}

func (c *fakeClient) Disconnect() {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.disconns++
	c.connected = false
}

func (c *fakeClient) IsConnected() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.connected
}

func (c *fakeClient) IsLoggedIn() bool { return c.IsConnected() && c.HasDevice() }

func (c *fakeClient) HasDevice() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.hasDevice
}

func (c *fakeClient) Self() wa.Self {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.self
}

func (c *fakeClient) Logout(context.Context) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.logouts++
	if c.logoutErr != nil {
		return c.logoutErr
	}
	c.hasDevice, c.connected = false, false
	return nil
}

func (c *fakeClient) DeleteDevice(context.Context) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.deletes++
	c.hasDevice = false
	return nil
}

func (c *fakeClient) GetQRChannel(context.Context) (<-chan whatsmeow.QRChannelItem, error) {
	if c.qrErr != nil {
		return nil, c.qrErr
	}
	return c.qr, nil
}

func (c *fakeClient) PairPhone(_ context.Context, phone string) (string, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.pairPhoneIn = phone
	return c.pairCode, c.pairErr
}

func (c *fakeClient) SendPasskeyResponse(_ context.Context, r *types.WebAuthnResponse) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.passkeys = append(c.passkeys, r)
	return c.passkeyErr
}

func (c *fakeClient) SendPasskeyConfirmation(context.Context) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.confirms++
	return c.confirmErr
}

func (c *fakeClient) WaitPreKeysUploaded(ctx context.Context, timeout time.Duration) error {
	c.mu.Lock()
	ch := c.prekeys
	c.mu.Unlock()
	if ch == nil {
		return nil
	}
	select {
	case <-ch:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	case <-time.After(timeout):
		return errors.New("prekeys timeout")
	}
}

func (c *fakeClient) SendPresence(_ context.Context, p types.Presence) error {
	select {
	case c.presences <- p:
	default:
	}
	return nil
}

func (c *fakeClient) SendChatPresence(_ context.Context, jid types.JID, st types.ChatPresence, _ types.ChatPresenceMedia) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.chatPres = append(c.chatPres, chatPresCall{jid, st})
	return nil
}

func (c *fakeClient) GenerateMessageID() types.MessageID {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.idCounter++
	return types.MessageID(fmt.Sprintf("3EB0FAKE%04d", c.idCounter))
}

func (c *fakeClient) SendMessage(_ context.Context, to types.JID, msg *waE2E.Message, extra whatsmeow.SendRequestExtra) (whatsmeow.SendResponse, error) {
	c.mu.Lock()
	c.sent = append(c.sent, sentMsg{to, msg, extra})
	fn := c.sendFn
	c.mu.Unlock()
	if fn != nil {
		return fn(to, msg, extra)
	}
	return whatsmeow.SendResponse{Timestamp: time.Unix(1700000500, 0), ID: extra.ID}, nil
}

func (c *fakeClient) BuildReaction(chat, sender types.JID, id types.MessageID, reaction string) *waE2E.Message {
	c.mu.Lock()
	c.reactions = append(c.reactions, reactionCall{chat, sender, id, reaction})
	c.mu.Unlock()
	return &waE2E.Message{ReactionMessage: &waE2E.ReactionMessage{
		Key:  &waCommon.MessageKey{RemoteJID: ptr(chat.String()), ID: ptr(id)},
		Text: ptr(reaction),
	}}
}

func (c *fakeClient) BuildHistorySyncRequest(anchor *types.MessageInfo, count int) *waE2E.Message {
	return &waE2E.Message{ProtocolMessage: &waE2E.ProtocolMessage{
		Type: waE2E.ProtocolMessage_PEER_DATA_OPERATION_REQUEST_MESSAGE.Enum(),
		PeerDataOperationRequestMessage: &waE2E.PeerDataOperationRequestMessage{
			HistorySyncOnDemandRequest: &waE2E.PeerDataOperationRequestMessage_HistorySyncOnDemandRequest{
				ChatJID:              ptr(anchor.Chat.String()),
				OldestMsgID:          ptr(anchor.ID),
				OldestMsgFromMe:      ptr(anchor.IsFromMe),
				OnDemandMsgCount:     ptr(int32(count)),
				OldestMsgTimestampMS: ptr(anchor.Timestamp.Unix()),
			},
		},
	}}
}

func (c *fakeClient) MarkRead(_ context.Context, ids []types.MessageID, _ time.Time, chat, sender types.JID) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.marks = append(c.marks, markCall{ids, chat, sender})
	return c.markErr
}

func (c *fakeClient) Upload(_ context.Context, _ []byte, mt whatsmeow.MediaType) (whatsmeow.UploadResponse, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.uploads = append(c.uploads, mt)
	return c.uploadResp, c.uploadErr
}

func (c *fakeClient) Download(_ context.Context, m whatsmeow.DownloadableMessage) ([]byte, error) {
	c.mu.Lock()
	c.downloads = append(c.downloads, m.GetDirectPath())
	fn := c.downloadFn
	c.mu.Unlock()
	if fn == nil {
		return nil, errors.New("no download scripted")
	}
	return fn(m)
}

func (c *fakeClient) SendMediaRetryReceipt(_ context.Context, info *types.MessageInfo, _ []byte) error {
	select {
	case c.receipts <- info:
	default:
	}
	return nil
}

func (c *fakeClient) DecryptMediaRetry(evt *events.MediaRetry, key []byte) (*waMmsRetry.MediaRetryNotification, error) {
	if c.decryptRet == nil {
		return nil, errors.New("no decrypt scripted")
	}
	return c.decryptRet(evt, key)
}

func (c *fakeClient) ParseWebMessage(chat types.JID, wm *waWeb.WebMessageInfo) (*events.Message, error) {
	info := types.MessageInfo{
		MessageSource: types.MessageSource{Chat: chat, IsFromMe: wm.GetKey().GetFromMe(), IsGroup: chat.Server == types.GroupServer},
		ID:            wm.GetKey().GetID(),
		PushName:      wm.GetPushName(),
		Timestamp:     time.Unix(int64(wm.GetMessageTimestamp()), 0),
	}
	switch {
	case info.IsFromMe:
		info.Sender = c.Self().JID
	case wm.GetKey().GetParticipant() != "":
		info.Sender = mustJID(wm.GetKey().GetParticipant())
	default:
		info.Sender = chat
	}
	return &events.Message{Info: info, Message: wm.GetMessage(), RawMessage: wm.GetMessage(), SourceWebMsg: wm}, nil
}

func (c *fakeClient) DecryptSecretEncryptedMessage(_ context.Context, evt *events.Message) (*waE2E.Message, error) {
	if c.decSecret == nil {
		return nil, errors.New("no decrypt scripted")
	}
	return c.decSecret(evt)
}

func (c *fakeClient) DecryptReaction(_ context.Context, evt *events.Message) (*waE2E.ReactionMessage, error) {
	if c.decReaction == nil {
		return nil, errors.New("no decrypt scripted")
	}
	return c.decReaction(evt)
}

func (c *fakeClient) GetGroupInfo(_ context.Context, jid types.JID) (*types.GroupInfo, error) {
	select {
	case c.groupCall <- jid:
	default:
	}
	if c.groupErr != nil {
		return nil, c.groupErr
	}
	return c.groupInfo, nil
}

func (c *fakeClient) GetContact(context.Context, types.JID) (types.ContactInfo, error) {
	return types.ContactInfo{}, nil
}

func (c *fakeClient) GetPNForLID(_ context.Context, lid types.JID) (types.JID, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if pn, found := c.lidToPN[lid.ToNonAD().String()]; found {
		return pn, nil
	}
	return types.EmptyJID, errors.New("unknown lid")
}

func (c *fakeClient) GetLIDForPN(_ context.Context, pn types.JID) (types.JID, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if lid, found := c.pnToLID[pn.ToNonAD().String()]; found {
		return lid, nil
	}
	return types.EmptyJID, errors.New("unknown pn")
}

func ptr[T any](v T) *T { return &v }

// fakeFactory hands out scripted clients and records how they were requested.
type fakeFactory struct {
	mu        sync.Mutex
	queue     []*fakeClient
	clients   []*fakeClient
	opts      []*wa.LoginOptions
	refreshes int
	closed    bool
	newErr    error
}

func (f *fakeFactory) NewClient(_ context.Context, opts *wa.LoginOptions) (wa.Client, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.newErr != nil {
		return nil, f.newErr
	}
	var c *fakeClient
	if len(f.queue) > 0 {
		c, f.queue = f.queue[0], f.queue[1:]
	} else {
		c = newFakeClient()
	}
	f.clients = append(f.clients, c)
	f.opts = append(f.opts, opts)
	return c, nil
}

func (f *fakeFactory) RefreshVersion(context.Context) {
	f.mu.Lock()
	f.refreshes++
	f.mu.Unlock()
}

func (f *fakeFactory) Close() error {
	f.mu.Lock()
	f.closed = true
	f.mu.Unlock()
	return nil
}

func (f *fakeFactory) client(i int) *fakeClient {
	f.mu.Lock()
	defer f.mu.Unlock()
	if i >= len(f.clients) {
		return nil
	}
	return f.clients[i]
}

func (f *fakeFactory) clientCount() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.clients)
}
