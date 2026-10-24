package wa

import (
	"context"
	"errors"
	"sync"
	"time"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/proto/waMmsRetry"
	"go.mau.fi/whatsmeow/proto/waWeb"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
)

// Real implements Client as thin pass-throughs over a whatsmeow client.
type Real struct {
	cli   *whatsmeow.Client
	scrub func(context.Context) error // wipes session.db residue once the device row is gone

	mu      sync.Mutex
	handler func(evt any) bool
	once    sync.Once
}

var _ Client = (*Real)(nil)

func newReal(cli *whatsmeow.Client, scrub func(context.Context) error) *Real {
	return &Real{cli: cli, scrub: scrub}
}

// Raw exposes the underlying client for diagnostics only.
func (r *Real) Raw() *whatsmeow.Client { return r.cli }

// SetEventHandler registers one trampoline on first use; later calls replace the target func.
// The trampoline turns a panic into a failed handler: whatsmeow would otherwise report success and acknowledge the event.
func (r *Real) SetEventHandler(h func(evt any) bool) {
	r.mu.Lock()
	r.handler = h
	r.mu.Unlock()
	r.once.Do(func() {
		r.cli.AddEventHandlerWithSuccessStatus(func(evt any) (handled bool) {
			defer func() {
				if recover() != nil {
					handled = false
				}
			}()
			r.mu.Lock()
			cur := r.handler
			r.mu.Unlock()
			if cur == nil {
				return true
			}
			return cur(evt)
		})
	})
}

func (r *Real) SetPrePairCallback(cb func(jid types.JID, platform, businessName string) bool) {
	r.cli.PrePairCallback = cb
}

func (r *Real) Connect() error    { return r.cli.Connect() }
func (r *Real) Disconnect()       { r.cli.Disconnect() }
func (r *Real) IsConnected() bool { return r.cli.IsConnected() }
func (r *Real) IsLoggedIn() bool  { return r.cli.IsLoggedIn() }

func (r *Real) HasDevice() bool { return r.cli.Store != nil && r.cli.Store.ID != nil }

// Self returns empty JIDs while the device is unpaired.
func (r *Real) Self() Self {
	if r.cli.Store == nil || r.cli.Store.ID == nil {
		return Self{}
	}
	return Self{
		JID:      r.cli.Store.ID.ToNonAD(),
		LID:      r.cli.Store.LID.ToNonAD(),
		PushName: r.cli.Store.PushName,
	}
}

func (r *Real) Logout(ctx context.Context) error { return r.cli.Logout(ctx) }

// DeleteDevice removes the device record, then the rows and freed pages that outlive it.
func (r *Real) DeleteDevice(ctx context.Context) error {
	err := r.cli.Store.Delete(ctx)
	if r.scrub != nil {
		err = errors.Join(err, r.scrub(ctx))
	}
	return err
}

func (r *Real) GetQRChannel(ctx context.Context) (<-chan whatsmeow.QRChannelItem, error) {
	return r.cli.GetQRChannel(ctx)
}

func (r *Real) PairPhone(ctx context.Context, phone string) (string, error) {
	return r.cli.PairPhone(ctx, phone, true, whatsmeow.PairClientChrome, PairDisplayName)
}

func (r *Real) SendPasskeyResponse(ctx context.Context, resp *types.WebAuthnResponse) error {
	return r.cli.SendPasskeyResponse(ctx, resp)
}

func (r *Real) SendPasskeyConfirmation(ctx context.Context) error {
	return r.cli.SendPasskeyConfirmation(ctx)
}

func (r *Real) WaitPreKeysUploaded(ctx context.Context, timeout time.Duration) error {
	return r.cli.PreKeysUploaded.WaitTimeoutCtx(ctx, timeout)
}

func (r *Real) SendPresence(ctx context.Context, p types.Presence) error {
	return r.cli.SendPresence(ctx, p)
}

func (r *Real) SendChatPresence(ctx context.Context, jid types.JID, state types.ChatPresence, media types.ChatPresenceMedia) error {
	return r.cli.SendChatPresence(ctx, jid, state, media)
}

func (r *Real) GenerateMessageID() types.MessageID { return r.cli.GenerateMessageID() }

func (r *Real) SendMessage(ctx context.Context, to types.JID, msg *waE2E.Message, extra whatsmeow.SendRequestExtra) (whatsmeow.SendResponse, error) {
	return r.cli.SendMessage(ctx, to, msg, extra)
}

func (r *Real) BuildReaction(chat, sender types.JID, id types.MessageID, reaction string) *waE2E.Message {
	return r.cli.BuildReaction(chat, sender, id, reaction)
}

func (r *Real) BuildHistorySyncRequest(anchor *types.MessageInfo, count int) *waE2E.Message {
	return r.cli.BuildHistorySyncRequest(anchor, count)
}

func (r *Real) MarkRead(ctx context.Context, ids []types.MessageID, ts time.Time, chat, sender types.JID) error {
	return r.cli.MarkRead(ctx, ids, ts, chat, sender)
}

func (r *Real) Upload(ctx context.Context, data []byte, mt whatsmeow.MediaType) (whatsmeow.UploadResponse, error) {
	return r.cli.Upload(ctx, data, mt)
}

func (r *Real) DownloadToFile(ctx context.Context, msg whatsmeow.DownloadableMessage, f whatsmeow.File) error {
	return r.cli.DownloadToFile(ctx, msg, f)
}

func (r *Real) SendMediaRetryReceipt(ctx context.Context, info *types.MessageInfo, mediaKey []byte) error {
	return r.cli.SendMediaRetryReceipt(ctx, info, mediaKey)
}

func (r *Real) DecryptMediaRetry(evt *events.MediaRetry, mediaKey []byte) (*waMmsRetry.MediaRetryNotification, error) {
	return whatsmeow.DecryptMediaRetryNotification(evt, mediaKey)
}

func (r *Real) ParseWebMessage(chat types.JID, msg *waWeb.WebMessageInfo) (*events.Message, error) {
	return r.cli.ParseWebMessage(chat, msg)
}

func (r *Real) DecryptSecretEncryptedMessage(ctx context.Context, evt *events.Message) (*waE2E.Message, error) {
	return r.cli.DecryptSecretEncryptedMessage(ctx, evt)
}

func (r *Real) DecryptReaction(ctx context.Context, evt *events.Message) (*waE2E.ReactionMessage, error) {
	return r.cli.DecryptReaction(ctx, evt)
}

func (r *Real) GetGroupInfo(ctx context.Context, jid types.JID) (*types.GroupInfo, error) {
	return r.cli.GetGroupInfo(ctx, jid)
}

func (r *Real) GetContact(ctx context.Context, jid types.JID) (types.ContactInfo, error) {
	return r.cli.Store.Contacts.GetContact(ctx, jid)
}

func (r *Real) GetPNForLID(ctx context.Context, lid types.JID) (types.JID, error) {
	return r.cli.Store.LIDs.GetPNForLID(ctx, lid)
}

func (r *Real) GetLIDForPN(ctx context.Context, pn types.JID) (types.JID, error) {
	return r.cli.Store.LIDs.GetLIDForPN(ctx, pn)
}
