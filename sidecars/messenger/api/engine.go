package api

import "context"

// State is the opaque session blob the daemon stores and hands back in hello.
type State struct {
	Cookies        map[string]string `json:"cookies"`
	UserAgent      string            `json:"user_agent"`
	FBID           string            `json:"fbid"`
	E2EERegistered bool              `json:"e2ee_registered"`
}

// Sink receives unsolicited events; data is one of the *Event structs above. It must not block.
type Sink func(event string, data any)

// Factory builds sessions. Open does no network I/O; dataDir is the hello data_dir.
// Wipe deletes the local session data under dataDir with or without a live session.
type Factory interface {
	Open(state State, dataDir string, sink Sink) (Session, error)
	Wipe(dataDir string) error
}

// Session is one account. Methods return *Error for every failure the client should see.
type Session interface {
	// Validate loads the messages page once and returns the account (login_finish).
	Validate(ctx context.Context) (Account, error)
	// Connect starts the realtime and E2EE connections; progress arrives as connection events.
	Connect(ctx context.Context) error
	Disconnect()
	// Logout disconnects and deletes local session data under dataDir (E2EE device, caches, thread map).
	Logout(ctx context.Context) error
	// Close releases resources without deleting data.
	Close()
	State() State

	ListThreads(ctx context.Context, p ListThreadsParams) (ListThreadsResult, error)
	FetchMessages(ctx context.Context, p FetchMessagesParams) (FetchMessagesResult, error)
	DownloadAttachment(ctx context.Context, p DownloadParams) (DownloadResult, error)
	SendText(ctx context.Context, p SendTextParams) (SendResult, error)
	SendMedia(ctx context.Context, p SendMediaParams) (SendResult, error)
	SendReaction(ctx context.Context, p SendReactionParams) error
	MarkRead(ctx context.Context, p MarkReadParams) error
	SetTyping(ctx context.Context, p SetTypingParams) error
}

// Params arrive already validated by the server (ranges, required fields, absolute paths).
type ListThreadsParams struct {
	Limit  int    `json:"limit"`
	Cursor string `json:"cursor,omitempty"`
}

type ListThreadsResult struct {
	Threads []Thread `json:"threads"`
	Cursor  string   `json:"cursor,omitempty"`
}

type FetchMessagesParams struct {
	ThreadID   string `json:"thread_id"`
	Limit      int    `json:"limit"`
	BeforeTSMS *int64 `json:"before_ts_ms,omitempty"`
	BeforeID   string `json:"before_id,omitempty"`
}

type FetchMessagesResult struct {
	Messages []Message `json:"messages"`
	HasMore  bool      `json:"has_more"`
}

type DownloadParams struct {
	ThreadID     string `json:"thread_id"`
	MessageID    string `json:"message_id"`
	AttachmentID string `json:"attachment_id"`
	DestPath     string `json:"dest_path"`
}

type DownloadResult struct {
	Path   string `json:"path"`
	Mime   string `json:"mime"`
	Bytes  int64  `json:"bytes"`
	SHA256 string `json:"sha256"`
}

type SendTextParams struct {
	ThreadID string `json:"thread_id"`
	Text     string `json:"text"`
	ReplyTo  string `json:"reply_to,omitempty"`
}

type SendMediaParams struct {
	ThreadID string `json:"thread_id"`
	Path     string `json:"path"`
	Mime     string `json:"mime"`
	Kind     string `json:"kind"`
	Caption  string `json:"caption,omitempty"`
	Filename string `json:"filename,omitempty"`
}

type SendResult struct {
	MessageID string `json:"message_id"`
	TS        int64  `json:"ts"`
}

type SendReactionParams struct {
	ThreadID  string `json:"thread_id"`
	MessageID string `json:"message_id"`
	Emoji     string `json:"emoji"`
}

type MarkReadParams struct {
	ThreadID string `json:"thread_id"`
	UpToTSMS *int64 `json:"up_to_ts_ms,omitempty"`
}

type SetTypingParams struct {
	ThreadID string `json:"thread_id"`
	On       bool   `json:"on"`
}
