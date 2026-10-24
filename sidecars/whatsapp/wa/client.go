// Package wa wraps the whatsmeow client behind an interface the server owns, so tests run without WhatsApp.
package wa

import (
	"context"
	"time"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/proto/waMmsRetry"
	"go.mau.fi/whatsmeow/proto/waWeb"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
)

// PairDisplayName and the Chrome client type are what PairPhone announces to the phone.
const PairDisplayName = "Chrome (Linux)"

// Self is the linked account as the device store knows it; JID is empty before pairing.
type Self struct {
	JID      types.JID // phone-number JID without device part
	LID      types.JID // LID without device part, may be empty
	PushName string
}

// LoginOptions shape store.DeviceProps before the first Connect of an unpaired device.
type LoginOptions struct {
	DeviceName  string
	FullHistory bool
}

// Client is every whatsmeow call the server makes; Real implements it, tests use a fake.
type Client interface {
	// Event handler: return true only once the event is committed and written to stdout.
	SetEventHandler(h func(evt any) bool)
	SetPrePairCallback(cb func(jid types.JID, platform, businessName string) bool)

	Connect() error
	Disconnect()
	IsConnected() bool
	IsLoggedIn() bool
	HasDevice() bool
	Self() Self
	Logout(ctx context.Context) error
	DeleteDevice(ctx context.Context) error

	GetQRChannel(ctx context.Context) (<-chan whatsmeow.QRChannelItem, error)
	PairPhone(ctx context.Context, phone string) (string, error)
	SendPasskeyResponse(ctx context.Context, resp *types.WebAuthnResponse) error
	SendPasskeyConfirmation(ctx context.Context) error
	WaitPreKeysUploaded(ctx context.Context, timeout time.Duration) error

	SendPresence(ctx context.Context, p types.Presence) error
	SendChatPresence(ctx context.Context, jid types.JID, state types.ChatPresence, media types.ChatPresenceMedia) error
	GenerateMessageID() types.MessageID
	SendMessage(ctx context.Context, to types.JID, msg *waE2E.Message, extra whatsmeow.SendRequestExtra) (whatsmeow.SendResponse, error)
	BuildReaction(chat, sender types.JID, id types.MessageID, reaction string) *waE2E.Message
	BuildHistorySyncRequest(anchor *types.MessageInfo, count int) *waE2E.Message
	MarkRead(ctx context.Context, ids []types.MessageID, ts time.Time, chat, sender types.JID) error
	Upload(ctx context.Context, data []byte, mt whatsmeow.MediaType) (whatsmeow.UploadResponse, error)
	// DownloadToFile streams the decrypted media into f; it never holds the whole file in memory.
	DownloadToFile(ctx context.Context, msg whatsmeow.DownloadableMessage, f whatsmeow.File) error
	SendMediaRetryReceipt(ctx context.Context, info *types.MessageInfo, mediaKey []byte) error
	DecryptMediaRetry(evt *events.MediaRetry, mediaKey []byte) (*waMmsRetry.MediaRetryNotification, error)

	ParseWebMessage(chat types.JID, msg *waWeb.WebMessageInfo) (*events.Message, error)
	DecryptSecretEncryptedMessage(ctx context.Context, evt *events.Message) (*waE2E.Message, error)
	DecryptReaction(ctx context.Context, evt *events.Message) (*waE2E.ReactionMessage, error)

	GetGroupInfo(ctx context.Context, jid types.JID) (*types.GroupInfo, error)
	GetContact(ctx context.Context, jid types.JID) (types.ContactInfo, error)
	GetPNForLID(ctx context.Context, lid types.JID) (types.JID, error)
	GetLIDForPN(ctx context.Context, pn types.JID) (types.JID, error)
}

// Factory owns session.db and hands out clients; NewClient does no network I/O.
type Factory interface {
	// NewClient builds a client on the stored device, or on a fresh unpaired one when none is stored.
	NewClient(ctx context.Context, opts *LoginOptions) (Client, error)
	// RefreshVersion asks WhatsApp for the current web version once per process; failures are ignored.
	RefreshVersion(ctx context.Context)
	// Scrub removes what a deleted device leaves in session.db: the LID map, privacy tokens and freed pages.
	Scrub(ctx context.Context) error
	Close() error
}
