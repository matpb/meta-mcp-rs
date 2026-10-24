package server

import (
	"bytes"
	"context"
	"image"
	"image/color"
	"image/draw"
	_ "image/gif"
	"image/jpeg"
	_ "image/png"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types"
	"google.golang.org/protobuf/proto"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/ipc"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/store"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/wa"
)

const (
	maxTextBytes  = 65536
	maxMediaBytes = 100 << 20
	thumbMaxSide  = 100
	thumbQuality  = 60
	unreadBatch   = 100
)

type sendTextParams struct {
	ThreadID string  `json:"thread_id"`
	Text     string  `json:"text"`
	ReplyTo  *string `json:"reply_to"`
}

// sendText sends a text message, optionally quoting a stored one.
func (s *server) sendText(ctx context.Context, req ipc.Request) ipc.Reply {
	var p sendTextParams
	if e := decode(req, &p); e != nil {
		return failErr(req.ID, e)
	}
	cl, chat, err := s.sendGate(ctx, p.ThreadID)
	if err != nil {
		return failErr(req.ID, err)
	}
	if len(p.Text) < 1 || len(p.Text) > maxTextBytes {
		return fail(req.ID, api.CodeBadParams, "text must be 1 to 65536 bytes")
	}
	to, perr := types.ParseJID(chat.JID)
	if perr != nil {
		return fail(req.ID, api.CodeInternal, "stored thread id is not a JID")
	}
	msg := &waE2E.Message{Conversation: proto.String(p.Text)}
	if p.ReplyTo != nil && *p.ReplyTo != "" {
		target, found, err := s.store.GetMessage(ctx, chat.JID, *p.ReplyTo)
		if err != nil {
			return fail(req.ID, api.CodeIO, "message database error")
		}
		if !found {
			return fail(req.ID, api.CodeMessageNotFound, "the message to reply to is not stored")
		}
		msg = &waE2E.Message{ExtendedTextMessage: &waE2E.ExtendedTextMessage{
			Text:        proto.String(p.Text),
			ContextInfo: replyContext(cl, target),
		}}
	}
	return s.deliver(ctx, req.ID, cl, chat, to, msg)
}

// replyContext builds the quote of a stored message.
func replyContext(cl wa.Client, target store.Msg) *waE2E.ContextInfo {
	participant := target.SenderWire
	if target.FromMe {
		participant = cl.Self().JID.ToNonAD().String()
	}
	ci := &waE2E.ContextInfo{StanzaID: proto.String(target.ID), Participant: proto.String(participant)}
	switch {
	case target.Text != nil:
		ci.QuotedMessage = &waE2E.Message{Conversation: proto.String(*target.Text)}
	case len(target.Raw) > 0:
		var q waE2E.Message
		if proto.Unmarshal(target.Raw, &q) == nil {
			ci.QuotedMessage = &q
		}
	}
	return ci
}

// deliver sends msg to the chat, stores the outgoing message and answers with its id and timestamp.
func (s *server) deliver(ctx context.Context, reqID uint64, cl wa.Client, chat store.Chat, to types.JID, msg *waE2E.Message) ipc.Reply {
	id := string(cl.GenerateMessageID())
	resp, err := cl.SendMessage(ctx, to, msg, whatsmeow.SendRequestExtra{ID: id})
	if err != nil {
		return failErr(reqID, s.sendError(err, id))
	}
	ts := resp.Timestamp.Unix()
	if resp.Timestamp.IsZero() {
		ts = time.Now().Unix()
	}
	self := cl.Self()
	o := origin{
		info:     types.MessageInfo{MessageSource: types.MessageSource{Chat: to, Sender: self.JID, IsFromMe: true}, ID: id, Timestamp: time.Unix(ts, 0)},
		chatID:   chat.JID,
		kind:     chat.Kind,
		senderID: self.JID.String(), senderName: self.PushName, self: self,
	}
	if !self.LID.IsEmpty() {
		o.senderAlt = self.LID.String()
	}
	m := s.msgFrom(o, chat.JID, msg)
	s.ingestMu.Lock()
	defer s.ingestMu.Unlock()
	if _, err := s.store.PutMessage(ctx, m, false); err != nil {
		s.log.Error().Msg("could not store a sent message")
	} else {
		s.emitLog(api.EvMessage, api.MessageEvent{Message: m.API(), Direction: "out"})
	}
	return ok(reqID, map[string]any{"message_id": id, "ts": ts})
}

type sendMediaParams struct {
	ThreadID string  `json:"thread_id"`
	Path     string  `json:"path"`
	Mime     string  `json:"mime"`
	Kind     string  `json:"kind"`
	Caption  *string `json:"caption"`
	Filename *string `json:"filename"`
}

// sendMedia uploads a local file and sends it as an image, video, audio or document message.
func (s *server) sendMedia(ctx context.Context, req ipc.Request) ipc.Reply {
	var p sendMediaParams
	if e := decode(req, &p); e != nil {
		return failErr(req.ID, e)
	}
	cl, chat, err := s.sendGate(ctx, p.ThreadID)
	if err != nil {
		return failErr(req.ID, err)
	}
	var mt whatsmeow.MediaType
	switch p.Kind {
	case "image":
		mt = whatsmeow.MediaImage
	case "video":
		mt = whatsmeow.MediaVideo
	case "audio":
		mt = whatsmeow.MediaAudio
	case "file":
		mt = whatsmeow.MediaDocument
	default:
		return fail(req.ID, api.CodeBadParams, "kind must be image, video, audio or file")
	}
	if !filepath.IsAbs(p.Path) {
		return fail(req.ID, api.CodeBadParams, "path must be absolute")
	}
	fi, serr := os.Stat(p.Path)
	if serr != nil || !fi.Mode().IsRegular() {
		return fail(req.ID, api.CodeBadParams, "path must be a readable regular file")
	}
	if fi.Size() > maxMediaBytes {
		return fail(req.ID, api.CodeBadParams, "file exceeds 100 MiB")
	}
	to, perr := types.ParseJID(chat.JID)
	if perr != nil {
		return fail(req.ID, api.CodeInternal, "stored thread id is not a JID")
	}
	data, rerr := os.ReadFile(p.Path)
	if rerr != nil {
		return fail(req.ID, api.CodeBadParams, "path must be a readable regular file")
	}
	mime := p.Mime
	if mime == "" {
		mime = http.DetectContentType(data[:min(len(data), 512)])
	}
	up, uerr := cl.Upload(ctx, data, mt)
	if uerr != nil {
		return failErr(req.ID, uploadError(uerr))
	}
	caption := ""
	if p.Caption != nil {
		caption = *p.Caption
	}
	name := filepath.Base(p.Path)
	if p.Filename != nil && *p.Filename != "" {
		name = *p.Filename
	}
	msg := buildMedia(p.Kind, mime, caption, name, data, up)
	return s.deliver(ctx, req.ID, cl, chat, to, msg)
}

func uploadError(err error) *api.Error {
	if e := api.AsError(err); e.Code != api.CodeInternal {
		return e
	}
	return api.Errorf(api.CodeSendFailed, "WhatsApp refused the upload")
}

func optStr(s string) *string {
	if s == "" {
		return nil
	}
	return proto.String(s)
}

// buildMedia assembles the WhatsApp message for an uploaded file.
func buildMedia(kind, mime, caption, name string, data []byte, up whatsmeow.UploadResponse) *waE2E.Message {
	switch kind {
	case "image":
		im := &waE2E.ImageMessage{
			URL: proto.String(up.URL), DirectPath: proto.String(up.DirectPath), MediaKey: up.MediaKey,
			FileEncSHA256: up.FileEncSHA256, FileSHA256: up.FileSHA256, FileLength: proto.Uint64(up.FileLength),
			Mimetype: proto.String(mime), Caption: optStr(caption),
		}
		if cfg, _, err := image.DecodeConfig(bytes.NewReader(data)); err == nil {
			im.Width, im.Height = proto.Uint32(uint32(cfg.Width)), proto.Uint32(uint32(cfg.Height))
		}
		im.JPEGThumbnail = thumbnail(data)
		return &waE2E.Message{ImageMessage: im}
	case "video":
		return &waE2E.Message{VideoMessage: &waE2E.VideoMessage{
			URL: proto.String(up.URL), DirectPath: proto.String(up.DirectPath), MediaKey: up.MediaKey,
			FileEncSHA256: up.FileEncSHA256, FileSHA256: up.FileSHA256, FileLength: proto.Uint64(up.FileLength),
			Mimetype: proto.String(mime), Caption: optStr(caption),
		}}
	case "audio":
		return &waE2E.Message{AudioMessage: &waE2E.AudioMessage{
			URL: proto.String(up.URL), DirectPath: proto.String(up.DirectPath), MediaKey: up.MediaKey,
			FileEncSHA256: up.FileEncSHA256, FileSHA256: up.FileSHA256, FileLength: proto.Uint64(up.FileLength),
			Mimetype: proto.String(mime), PTT: proto.Bool(isVoiceNote(mime)),
		}}
	}
	return &waE2E.Message{DocumentMessage: &waE2E.DocumentMessage{
		URL: proto.String(up.URL), DirectPath: proto.String(up.DirectPath), MediaKey: up.MediaKey,
		FileEncSHA256: up.FileEncSHA256, FileSHA256: up.FileSHA256, FileLength: proto.Uint64(up.FileLength),
		Mimetype: proto.String(mime), FileName: proto.String(name), Title: proto.String(name), Caption: optStr(caption),
	}}
}

// isVoiceNote is true for Ogg Opus audio, which WhatsApp plays as a voice note.
func isVoiceNote(mime string) bool {
	m := strings.ToLower(strings.ReplaceAll(mime, " ", ""))
	return strings.HasPrefix(m, "audio/ogg") && strings.Contains(m, "codecs=opus")
}

// thumbnail returns a small JPEG of an image, or nil when the data cannot be decoded.
func thumbnail(data []byte) []byte {
	src, _, err := image.Decode(bytes.NewReader(data))
	if err != nil {
		return nil
	}
	b := src.Bounds()
	w, h := b.Dx(), b.Dy()
	if w < 1 || h < 1 {
		return nil
	}
	tw, th := w, h
	if long := max(w, h); long > thumbMaxSide {
		tw, th = max(1, w*thumbMaxSide/long), max(1, h*thumbMaxSide/long)
	}
	dst := image.NewRGBA(image.Rect(0, 0, tw, th))
	draw.Draw(dst, dst.Bounds(), image.White, image.Point{}, draw.Src)
	for y := 0; y < th; y++ {
		for x := 0; x < tw; x++ {
			r, g, bl, a := src.At(b.Min.X+x*w/tw, b.Min.Y+y*h/th).RGBA()
			if a == 0 {
				continue
			}
			white := 0xffff - a
			dst.SetRGBA(x, y, color.RGBA{R: uint8((r + white) >> 8), G: uint8((g + white) >> 8), B: uint8((bl + white) >> 8), A: 255})
		}
	}
	var buf bytes.Buffer
	if jpeg.Encode(&buf, dst, &jpeg.Options{Quality: thumbQuality}) != nil {
		return nil
	}
	return buf.Bytes()
}

type sendReactionParams struct {
	ThreadID  string `json:"thread_id"`
	MessageID string `json:"message_id"`
	Emoji     string `json:"emoji"`
}

// sendReaction reacts to a stored message; an empty emoji removes the reaction.
func (s *server) sendReaction(ctx context.Context, req ipc.Request) ipc.Reply {
	var p sendReactionParams
	if e := decode(req, &p); e != nil {
		return failErr(req.ID, e)
	}
	cl, chat, err := s.sendGate(ctx, p.ThreadID)
	if err != nil {
		return failErr(req.ID, err)
	}
	if p.MessageID == "" {
		return fail(req.ID, api.CodeBadParams, "message_id is required")
	}
	target, found, gerr := s.store.GetMessage(ctx, chat.JID, p.MessageID)
	if gerr != nil {
		return fail(req.ID, api.CodeIO, "message database error")
	}
	if !found {
		return fail(req.ID, api.CodeMessageNotFound, "the message is not stored")
	}
	to, perr := types.ParseJID(chat.JID)
	if perr != nil {
		return fail(req.ID, api.CodeInternal, "stored thread id is not a JID")
	}
	self := cl.Self()
	sender := self.JID.ToNonAD()
	if !target.FromMe {
		if sender, perr = types.ParseJID(target.SenderWire); perr != nil {
			return fail(req.ID, api.CodeInternal, "stored sender is not a JID")
		}
	}
	msg := cl.BuildReaction(to, sender, types.MessageID(p.MessageID), p.Emoji)
	if _, err := cl.SendMessage(ctx, to, msg, whatsmeow.SendRequestExtra{}); err != nil {
		return failErr(req.ID, s.sendError(err, ""))
	}
	s.ingestMu.Lock()
	defer s.ingestMu.Unlock()
	if err := s.store.PutReaction(ctx, chat.JID, p.MessageID, self.JID.String(), p.Emoji, time.Now().Unix()); err != nil {
		s.log.Error().Msg("could not store a sent reaction")
	}
	return ok(req.ID, struct{}{})
}

type markReadParams struct {
	ThreadID string `json:"thread_id"`
	UpToMS   *int64 `json:"up_to_ts_ms"`
}

// markRead sends read receipts for stored unread incoming messages, grouped by sender.
func (s *server) markRead(ctx context.Context, req ipc.Request) ipc.Reply {
	var p markReadParams
	if e := decode(req, &p); e != nil {
		return failErr(req.ID, e)
	}
	cl, chat, err := s.sendGate(ctx, p.ThreadID)
	if err != nil {
		return failErr(req.ID, err)
	}
	upTo := int64(1<<62 - 1)
	if p.UpToMS != nil {
		upTo = *p.UpToMS / 1000
	}
	msgs, merr := s.allUnread(ctx, chat, upTo)
	if merr != nil {
		return fail(req.ID, api.CodeIO, "message database error")
	}
	if len(msgs) == 0 {
		return ok(req.ID, struct{}{})
	}
	var order []string
	groups := map[string][]store.Msg{}
	for _, m := range msgs {
		if _, seen := groups[m.SenderWire]; !seen {
			order = append(order, m.SenderWire)
		}
		groups[m.SenderWire] = append(groups[m.SenderWire], m)
	}
	for _, wire := range order {
		g := groups[wire]
		chatWire, err1 := types.ParseJID(g[0].ChatWire)
		if err1 != nil {
			return fail(req.ID, api.CodeInternal, "stored chat is not a JID")
		}
		sender := types.EmptyJID
		if chat.Kind == api.KindGroup {
			if sender, err1 = types.ParseJID(wire); err1 != nil {
				return fail(req.ID, api.CodeInternal, "stored sender is not a JID")
			}
		}
		ids := make([]types.MessageID, len(g))
		for i, m := range g {
			ids[i] = types.MessageID(m.ID)
		}
		if err := cl.MarkRead(ctx, ids, time.Now(), chatWire, sender); err != nil {
			return failErr(req.ID, s.sendError(err, ""))
		}
	}
	s.ingestMu.Lock()
	defer s.ingestMu.Unlock()
	upd, _, uerr := s.store.UpdateChat(ctx, chat.JID, func(c *store.Chat) {
		if msgs[0].TS > c.ReadUpToTS {
			c.ReadUpToTS = msgs[0].TS
		}
		if p.UpToMS == nil {
			c.UnreadCount = 0
		} else {
			c.UnreadCount = max(0, c.UnreadCount-len(msgs))
		}
	})
	if uerr != nil {
		return fail(req.ID, api.CodeIO, "message database error")
	}
	s.emitLog(api.EvThread, api.ThreadEvent{Thread: upd.API()})
	return ok(req.ID, struct{}{})
}

type setTypingParams struct {
	ThreadID string `json:"thread_id"`
	On       bool   `json:"on"`
}

// setTyping sends a composing or paused chat state.
func (s *server) setTyping(ctx context.Context, req ipc.Request) ipc.Reply {
	var p setTypingParams
	if e := decode(req, &p); e != nil {
		return failErr(req.ID, e)
	}
	cl, chat, err := s.sendGate(ctx, p.ThreadID)
	if err != nil {
		return failErr(req.ID, err)
	}
	to, perr := types.ParseJID(chat.JID)
	if perr != nil {
		return fail(req.ID, api.CodeInternal, "stored thread id is not a JID")
	}
	state := types.ChatPresencePaused
	if p.On {
		state = types.ChatPresenceComposing
	}
	if err := cl.SendChatPresence(ctx, to, state, ""); err != nil {
		return failErr(req.ID, s.sendError(err, ""))
	}
	return ok(req.ID, struct{}{})
}

// allUnread collects every unread incoming message up to upTo, newest first, 100 rows per query.
func (s *server) allUnread(ctx context.Context, chat store.Chat, upTo int64) ([]store.Msg, error) {
	var all []store.Msg
	seen := map[string]bool{}
	upper := upTo
	for {
		batch, err := s.store.UnreadIncoming(ctx, chat.JID, chat.ReadUpToTS, upper, unreadBatch)
		if err != nil {
			return nil, err
		}
		fresh := 0
		for _, m := range batch {
			if !seen[m.ID] {
				seen[m.ID] = true
				all = append(all, m)
				fresh++
			}
		}
		if len(batch) < unreadBatch {
			return all, nil
		}
		upper = batch[len(batch)-1].TS
		if fresh == 0 {
			upper--
		}
	}
}
