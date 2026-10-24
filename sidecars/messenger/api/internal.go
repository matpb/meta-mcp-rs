package api

// The types below are shared by normalize, e2ee and engine; they never go on the wire.

// Edit replaces the text of an existing message. SenderID is the authenticated sender of the
// edit event and TargetSenderID the sender named by the message key; both are decimal ids.
type Edit struct {
	ThreadID       string
	MessageID      string
	Text           string
	TS             int64
	SenderID       string
	TargetSenderID string
}

// Removal is an unsent message (Unsent true, for everyone) or a delete for this account only.
// ByAdmin is set by the engine when the sender is a known admin of the group.
type Removal struct {
	ThreadID       string
	MessageID      string
	Unsent         bool
	SenderID       string
	TargetSenderID string
	ByAdmin        bool
}

// E2EEMedia is what download_attachment needs to fetch one encrypted attachment later.
type E2EEMedia struct {
	MessageID    string
	AttachmentID string
	MediaType    string // whatsmeow.MediaType value
	Integral     []byte // proto.Marshal of *waMediaTransport.WAMediaTransport_Integral
	Mime         string
	Size         int64
}

// SendOutcome is the server's answer to an optimistic send, keyed by offline threading id.
type SendOutcome struct {
	OTID      string
	MessageID string // "fb:<mid>" on success
	Failed    bool
	Reason    string // server text, never logged verbatim
}

// MessageRange is the paging state the server reports for one thread.
type MessageRange struct {
	ThreadID      string
	FBKey         int64
	MinTSMS       int64
	MinMessageID  string
	HasMoreBefore bool
}
