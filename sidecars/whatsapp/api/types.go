// Package api holds the wire shapes of protocol 1 (WhatsApp flavour, Amendment 1 included).
package api

// Account is the linked WhatsApp account; ID is the phone-number JID.
type Account struct {
	ID   string `json:"id"`
	Name string `json:"name"`
}

// Participant is one member of a thread.
type Participant struct {
	ID   string `json:"id"`
	Name string `json:"name"`
	IsMe bool   `json:"is_me"`
}

// Thread kinds.
const (
	KindDM    = "dm"
	KindGroup = "group"
)

// Thread is one chat; ID is the canonical JID (phone-number form for DMs when known), AltID the other form.
type Thread struct {
	ID                 string        `json:"id"`
	AltID              *string       `json:"alt_id"`
	Kind               string        `json:"kind"`
	Name               *string       `json:"name"`
	Participants       []Participant `json:"participants"`
	LastMessageTS      *int64        `json:"last_message_ts"`
	LastMessagePreview *string       `json:"last_message_preview"`
	Unread             *bool         `json:"unread"`
	Encrypted          bool          `json:"encrypted"`
}

// Sender identifies who wrote a message.
type Sender struct {
	ID   string `json:"id"`
	Name string `json:"name"`
	IsMe bool   `json:"is_me"`
}

// Reaction is one user's reaction on a message.
type Reaction struct {
	SenderID string `json:"sender_id"`
	Emoji    string `json:"emoji"`
}

// Attachment kinds.
const (
	AttImage    = "image"
	AttVideo    = "video"
	AttAudio    = "audio"
	AttFile     = "file"
	AttSticker  = "sticker"
	AttGIF      = "gif"
	AttLink     = "link"
	AttLocation = "location"
	AttOther    = "other"
)

// Attachment describes media or a share carried by a message; bytes come from download_attachment.
type Attachment struct {
	ID         string  `json:"id"`
	Kind       string  `json:"kind"`
	Mime       *string `json:"mime"`
	Name       *string `json:"name"`
	Size       *int64  `json:"size"`
	Width      *int64  `json:"width,omitempty"`
	Height     *int64  `json:"height,omitempty"`
	DurationMS *int64  `json:"duration_ms,omitempty"`
	URL        *string `json:"url"`
	ExpiresAt  *int64  `json:"expires_at"`
}

// Message kinds.
const (
	MsgText        = "text"
	MsgMedia       = "media"
	MsgSticker     = "sticker"
	MsgLink        = "link"
	MsgLocation    = "location"
	MsgCall        = "call"
	MsgSystem      = "system"
	MsgUnsupported = "unsupported"
)

// Message is one normalised message; ID is the WhatsApp message id, unique per chat.
type Message struct {
	ID          string       `json:"id"`
	ThreadID    string       `json:"thread_id"`
	TS          int64        `json:"ts"`
	Sender      Sender       `json:"sender"`
	Text        *string      `json:"text"`
	Attachments []Attachment `json:"attachments"`
	ReplyTo     *string      `json:"reply_to"`
	Reactions   []Reaction   `json:"reactions"`
	Edited      bool         `json:"edited"`
	Unsent      bool         `json:"unsent"`
	Encrypted   bool         `json:"encrypted"`
	Kind        string       `json:"kind"`
}

// Event names.
const (
	EvReady        = "ready"
	EvQR           = "qr"
	EvPasskeyReq   = "passkey_request"
	EvPasskeyConf  = "passkey_confirmation"
	EvConnection   = "connection"
	EvLoggedIn     = "logged_in"
	EvLoggedOut    = "logged_out"
	EvStateUpdated = "state_updated"
	EvMessage      = "message"
	EvReaction     = "reaction"
	EvReceipt      = "receipt"
	EvThread       = "thread"
	EvHistory      = "history"
	EvError        = "error"
)

// Connection states and actions.
const (
	ConnConnecting   = "connecting"
	ConnConnected    = "connected"
	ConnDisconnected = "disconnected"
	ConnFatal        = "fatal"

	ActionRetry   = "retry"
	ActionRelogin = "relogin"
	ActionWait    = "wait"
	ActionNone    = "none"
)

type ConnectionEvent struct {
	State  string `json:"state"`
	Reason string `json:"reason,omitempty"`
	Action string `json:"action,omitempty"`
}

type LoggedInEvent struct {
	Account Account `json:"account"`
}

type LoggedOutEvent struct {
	Reason string `json:"reason"`
	URL    string `json:"url,omitempty"`
}

// Fatal connection reasons (Amendment 1); any of them blocks sends until restart.
const (
	FatalTemporaryBan     = "temporary_ban"
	FatalReachoutTimelock = "reachout_timelock"
	FatalClientOutdated   = "client_outdated"
	FatalStreamReplaced   = "stream_replaced"
	FatalDeviceRemoved    = "device_removed"
	FatalBanned           = "banned"
)

// State is the metadata blob passed through hello and state_updated; keys stay in session.db.
type State struct {
	JID         string `json:"jid"`
	LID         string `json:"lid"`
	PushName    string `json:"push_name"`
	PairedAt    int64  `json:"paired_at"`
	FullHistory bool   `json:"full_history"`
}

type QREvent struct {
	Code       string `json:"code"`
	ExpiresInS int    `json:"expires_in_s"`
}

// PasskeyRequestEvent carries the library's WebAuthn request options verbatim.
type PasskeyRequestEvent struct {
	PublicKey    any    `json:"public_key"`
	Instructions string `json:"instructions"`
}

type PasskeyConfirmationEvent struct {
	Code string `json:"code"`
}

// StateUpdatedEvent carries the blob the daemon persists; State nil means forget the session.
type StateUpdatedEvent struct {
	State *State `json:"state"`
}

type MessageEvent struct {
	Message   Message `json:"message"`
	Direction string  `json:"direction"`
}

type ReactionEvent struct {
	ThreadID  string  `json:"thread_id"`
	MessageID string  `json:"message_id"`
	SenderID  string  `json:"sender_id"`
	Emoji     *string `json:"emoji"`
	TS        int64   `json:"ts"`
}

type ReceiptEvent struct {
	ThreadID string `json:"thread_id"`
	Kind     string `json:"kind"`
	SenderID string `json:"sender_id"`
	UpToTS   int64  `json:"up_to_ts"`
}

type ThreadEvent struct {
	Thread Thread `json:"thread"`
}

type HistoryEvent struct {
	ThreadID string    `json:"thread_id"`
	Messages []Message `json:"messages"`
	Complete bool      `json:"complete"`
}

type ErrorEvent struct {
	Code    string `json:"code"`
	Message string `json:"message"`
}
