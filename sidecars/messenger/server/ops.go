package server

import (
	"context"
	"os"
	"path/filepath"
	"unicode/utf8"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
	"github.com/matpb/meta-mcp-rs/sidecars/messenger/ipc"
)

type handler func(ctx context.Context, req ipc.Request) ipc.Reply

const (
	maxIDBytes   = 128
	maxLongID    = 512
	maxText      = 20000
	maxCursor    = 4096
	maxBeforeID  = 256
	maxEmoji     = 64
	maxFilename  = 255
	maxMime      = 255
	defaultLimit = 50
)

// asyncHandler returns the handler for a session-bound op, or nil for an unknown op.
func (s *server) asyncHandler(op string) handler {
	if h, found := s.hooks.extra[op]; found {
		return h
	}
	switch op {
	case "logout":
		return s.logout
	case "connect":
		return s.connect
	case "disconnect":
		return s.disconnect
	case "list_threads":
		return s.listThreads
	case "fetch_messages":
		return s.fetchMessages
	case "download_attachment":
		return s.downloadAttachment
	case "send_text":
		return s.sendText
	case "send_media":
		return s.sendMedia
	case "send_reaction":
		return s.sendReaction
	case "mark_read":
		return s.markRead
	case "set_typing":
		return s.setTyping
	}
	return nil
}

func badParam(id uint64, field string) ipc.Reply {
	return fail(id, api.CodeBadParams, "invalid "+field)
}

func noSession(id uint64) ipc.Reply {
	return fail(id, api.CodeNotLoggedIn, "not logged in")
}

func (s *server) connect(ctx context.Context, req ipc.Request) ipc.Reply {
	sess := s.session()
	if sess == nil {
		return noSession(req.ID)
	}
	if err := sess.Connect(ctx); err != nil {
		return failErr(req.ID, err)
	}
	return ok(req.ID, struct{}{})
}

func (s *server) disconnect(_ context.Context, req ipc.Request) ipc.Reply {
	if sess := s.session(); sess != nil {
		sess.Disconnect()
	}
	return ok(req.ID, struct{}{})
}

func (s *server) listThreads(ctx context.Context, req ipc.Request) ipc.Reply {
	var p struct {
		Limit  *int   `json:"limit"`
		Cursor string `json:"cursor"`
	}
	if !decodeParams(req.Params, &p) {
		return badParam(req.ID, "params")
	}
	limit, good := rangeLimit(p.Limit, defaultLimit, 200)
	if !good {
		return badParam(req.ID, "limit")
	}
	if len(p.Cursor) > maxCursor {
		return badParam(req.ID, "cursor")
	}
	sess := s.session()
	if sess == nil {
		return noSession(req.ID)
	}
	res, err := sess.ListThreads(ctx, api.ListThreadsParams{Limit: limit, Cursor: p.Cursor})
	if err != nil {
		return failErr(req.ID, err)
	}
	if res.Threads == nil {
		res.Threads = []api.Thread{}
	}
	for i := range res.Threads {
		if res.Threads[i].Participants == nil {
			res.Threads[i].Participants = []api.Participant{}
		}
	}
	return ok(req.ID, res)
}

func (s *server) fetchMessages(ctx context.Context, req ipc.Request) ipc.Reply {
	var p struct {
		ThreadID   string `json:"thread_id"`
		Limit      *int   `json:"limit"`
		BeforeTSMS *int64 `json:"before_ts_ms"`
		BeforeID   string `json:"before_id"`
	}
	if !decodeParams(req.Params, &p) {
		return badParam(req.ID, "params")
	}
	if !idOK(p.ThreadID, maxIDBytes) {
		return badParam(req.ID, "thread_id")
	}
	limit, good := rangeLimit(p.Limit, defaultLimit, 100)
	if !good {
		return badParam(req.ID, "limit")
	}
	if p.BeforeTSMS != nil && *p.BeforeTSMS < 0 {
		return badParam(req.ID, "before_ts_ms")
	}
	if len(p.BeforeID) > maxBeforeID {
		return badParam(req.ID, "before_id")
	}
	sess := s.session()
	if sess == nil {
		return noSession(req.ID)
	}
	res, err := sess.FetchMessages(ctx, api.FetchMessagesParams{
		ThreadID: p.ThreadID, Limit: limit, BeforeTSMS: p.BeforeTSMS, BeforeID: p.BeforeID,
	})
	if err != nil {
		return failErr(req.ID, err)
	}
	if res.Messages == nil {
		res.Messages = []api.Message{}
	}
	for i := range res.Messages {
		m := &res.Messages[i]
		if m.Attachments == nil {
			m.Attachments = []api.Attachment{}
		}
		if m.Reactions == nil {
			m.Reactions = []api.Reaction{}
		}
	}
	return ok(req.ID, res)
}

func (s *server) downloadAttachment(ctx context.Context, req ipc.Request) ipc.Reply {
	var p api.DownloadParams
	if !decodeParams(req.Params, &p) {
		return badParam(req.ID, "params")
	}
	switch {
	case !idOK(p.ThreadID, maxIDBytes):
		return badParam(req.ID, "thread_id")
	case !idOK(p.MessageID, maxLongID):
		return badParam(req.ID, "message_id")
	case !idOK(p.AttachmentID, maxLongID):
		return badParam(req.ID, "attachment_id")
	case !destPathOK(p.DestPath):
		return badParam(req.ID, "dest_path")
	}
	sess := s.session()
	if sess == nil {
		return noSession(req.ID)
	}
	res, err := sess.DownloadAttachment(ctx, p)
	if err != nil {
		return failErr(req.ID, err)
	}
	return ok(req.ID, res)
}

func (s *server) sendText(ctx context.Context, req ipc.Request) ipc.Reply {
	var p api.SendTextParams
	if !decodeParams(req.Params, &p) {
		return badParam(req.ID, "params")
	}
	switch {
	case !idOK(p.ThreadID, maxIDBytes):
		return badParam(req.ID, "thread_id")
	case !textOK(p.Text, 1, maxText):
		return badParam(req.ID, "text")
	case len(p.ReplyTo) > maxBeforeID:
		return badParam(req.ID, "reply_to")
	}
	sess := s.session()
	if sess == nil {
		return noSession(req.ID)
	}
	res, err := sess.SendText(ctx, p)
	if err != nil {
		return failErr(req.ID, err)
	}
	return ok(req.ID, res)
}

func (s *server) sendMedia(ctx context.Context, req ipc.Request) ipc.Reply {
	var p api.SendMediaParams
	if !decodeParams(req.Params, &p) {
		return badParam(req.ID, "params")
	}
	switch {
	case !idOK(p.ThreadID, maxIDBytes):
		return badParam(req.ID, "thread_id")
	case !readableFile(p.Path):
		return badParam(req.ID, "path")
	case p.Kind != "image" && p.Kind != "video" && p.Kind != "audio" && p.Kind != "file":
		return badParam(req.ID, "kind")
	case len(p.Mime) < 1 || len(p.Mime) > maxMime:
		return badParam(req.ID, "mime")
	case !textOK(p.Caption, 0, maxText):
		return badParam(req.ID, "caption")
	case !filenameOK(p.Filename):
		return badParam(req.ID, "filename")
	}
	sess := s.session()
	if sess == nil {
		return noSession(req.ID)
	}
	res, err := sess.SendMedia(ctx, p)
	if err != nil {
		return failErr(req.ID, err)
	}
	return ok(req.ID, res)
}

func (s *server) sendReaction(ctx context.Context, req ipc.Request) ipc.Reply {
	var p api.SendReactionParams
	if !decodeParams(req.Params, &p) {
		return badParam(req.ID, "params")
	}
	switch {
	case !idOK(p.ThreadID, maxIDBytes):
		return badParam(req.ID, "thread_id")
	case !idOK(p.MessageID, maxLongID):
		return badParam(req.ID, "message_id")
	case len(p.Emoji) > maxEmoji || !utf8.ValidString(p.Emoji):
		return badParam(req.ID, "emoji")
	}
	sess := s.session()
	if sess == nil {
		return noSession(req.ID)
	}
	if err := sess.SendReaction(ctx, p); err != nil {
		return failErr(req.ID, err)
	}
	return ok(req.ID, struct{}{})
}

func (s *server) markRead(ctx context.Context, req ipc.Request) ipc.Reply {
	var p api.MarkReadParams
	if !decodeParams(req.Params, &p) {
		return badParam(req.ID, "params")
	}
	switch {
	case !idOK(p.ThreadID, maxIDBytes):
		return badParam(req.ID, "thread_id")
	case p.UpToTSMS != nil && *p.UpToTSMS < 0:
		return badParam(req.ID, "up_to_ts_ms")
	}
	sess := s.session()
	if sess == nil {
		return noSession(req.ID)
	}
	if err := sess.MarkRead(ctx, p); err != nil {
		return failErr(req.ID, err)
	}
	return ok(req.ID, struct{}{})
}

func (s *server) setTyping(ctx context.Context, req ipc.Request) ipc.Reply {
	var p struct {
		ThreadID string `json:"thread_id"`
		On       *bool  `json:"on"`
	}
	if !decodeParams(req.Params, &p) {
		return badParam(req.ID, "params")
	}
	switch {
	case !idOK(p.ThreadID, maxIDBytes):
		return badParam(req.ID, "thread_id")
	case p.On == nil:
		return badParam(req.ID, "on")
	}
	sess := s.session()
	if sess == nil {
		return noSession(req.ID)
	}
	if err := sess.SetTyping(ctx, api.SetTypingParams{ThreadID: p.ThreadID, On: *p.On}); err != nil {
		return failErr(req.ID, err)
	}
	return ok(req.ID, struct{}{})
}

// rangeLimit applies the default for an absent limit and checks 1..max.
func rangeLimit(v *int, def, max int) (int, bool) {
	if v == nil {
		return def, true
	}
	return *v, *v >= 1 && *v <= max
}

func idOK(v string, max int) bool {
	return len(v) >= 1 && len(v) <= max
}

func textOK(v string, min, max int) bool {
	n := utf8.RuneCountInString(v)
	return utf8.ValidString(v) && n >= min && n <= max
}

func isAbsClean(p string) bool {
	return filepath.IsAbs(p) && filepath.Clean(p) == p
}

func destPathOK(p string) bool {
	if !isAbsClean(p) {
		return false
	}
	if fi, err := os.Stat(filepath.Dir(p)); err != nil || !fi.IsDir() {
		return false
	}
	if fi, err := os.Stat(p); err == nil && fi.IsDir() {
		return false
	}
	return true
}

func readableFile(p string) bool {
	if !filepath.IsAbs(p) {
		return false
	}
	fi, err := os.Stat(p)
	if err != nil || !fi.Mode().IsRegular() {
		return false
	}
	f, err := os.Open(p)
	if err != nil {
		return false
	}
	_ = f.Close()
	return true
}

func filenameOK(n string) bool {
	if len(n) > maxFilename {
		return false
	}
	for i := 0; i < len(n); i++ {
		if n[i] == '/' || n[i] == 0 {
			return false
		}
	}
	return true
}
