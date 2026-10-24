package server

import (
	"context"
	"errors"
	"strings"
	"sync"
	"time"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/proto/waMmsRetry"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	"google.golang.org/protobuf/proto"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/ipc"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/normalize"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/store"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/wa"
)

type mediaRetryResult struct{ evt *events.MediaRetry }

// mediaWaiters routes MediaRetry events to the downloads waiting on that chat and message id.
type mediaWaiters struct {
	mu sync.Mutex
	m  map[string][]chan *mediaRetryResult
}

// mediaKey identifies a retry request by the wire chat and message id.
func mediaKey(chatWire, id string) string {
	if j, err := types.ParseJID(chatWire); err == nil {
		chatWire = j.ToNonAD().String()
	}
	return chatWire + "\x00" + id
}

func (w *mediaWaiters) register(key string) chan *mediaRetryResult {
	ch := make(chan *mediaRetryResult, 1)
	w.mu.Lock()
	w.m[key] = append(w.m[key], ch)
	w.mu.Unlock()
	return ch
}

func (w *mediaWaiters) unregister(key string, ch chan *mediaRetryResult) {
	w.mu.Lock()
	defer w.mu.Unlock()
	rest := w.m[key][:0:0]
	for _, c := range w.m[key] {
		if c != ch {
			rest = append(rest, c)
		}
	}
	if len(rest) == 0 {
		delete(w.m, key)
	} else {
		w.m[key] = rest
	}
}

// deliver wakes every waiter of the event's message; an event without a chat matches by id alone.
func (w *mediaWaiters) deliver(evt *events.MediaRetry) {
	w.mu.Lock()
	var targets []chan *mediaRetryResult
	if evt.ChatID.IsEmpty() {
		for k, chs := range w.m {
			if strings.HasSuffix(k, "\x00"+evt.MessageID) {
				targets = append(targets, chs...)
			}
		}
	} else {
		targets = append(targets, w.m[mediaKey(evt.ChatID.String(), evt.MessageID)]...)
	}
	w.mu.Unlock()
	for _, ch := range targets {
		select {
		case ch <- &mediaRetryResult{evt: evt}:
		default:
		}
	}
}

type downloadParams struct {
	ThreadID     string `json:"thread_id"`
	MessageID    string `json:"message_id"`
	AttachmentID string `json:"attachment_id"`
	DestPath     string `json:"dest_path"`
}

// downloadable is the media sub-message of a stored message.
type downloadable struct {
	msg    whatsmeow.DownloadableMessage
	mime   string
	length uint64 // the size the sender advertised, 0 when absent
	setDir func(string)
}

func pickDownloadable(m *waE2E.Message) *downloadable {
	switch {
	case m.GetImageMessage() != nil:
		x := m.GetImageMessage()
		return &downloadable{x, x.GetMimetype(), x.GetFileLength(), func(p string) { x.DirectPath = proto.String(p) }}
	case m.GetVideoMessage() != nil:
		x := m.GetVideoMessage()
		return &downloadable{x, x.GetMimetype(), x.GetFileLength(), func(p string) { x.DirectPath = proto.String(p) }}
	case m.GetPtvMessage() != nil:
		x := m.GetPtvMessage()
		return &downloadable{x, x.GetMimetype(), x.GetFileLength(), func(p string) { x.DirectPath = proto.String(p) }}
	case m.GetAudioMessage() != nil:
		x := m.GetAudioMessage()
		return &downloadable{x, x.GetMimetype(), x.GetFileLength(), func(p string) { x.DirectPath = proto.String(p) }}
	case m.GetDocumentMessage() != nil:
		x := m.GetDocumentMessage()
		return &downloadable{x, x.GetMimetype(), x.GetFileLength(), func(p string) { x.DirectPath = proto.String(p) }}
	case m.GetStickerMessage() != nil:
		x := m.GetStickerMessage()
		return &downloadable{x, x.GetMimetype(), x.GetFileLength(), func(p string) { x.DirectPath = proto.String(p) }}
	}
	return nil
}

// isExpiredMedia reports a CDN refusal that a fresh upload from the phone can fix.
func isExpiredMedia(err error) bool {
	return errors.Is(err, whatsmeow.ErrMediaDownloadFailedWith404) ||
		errors.Is(err, whatsmeow.ErrMediaDownloadFailedWith410) ||
		errors.Is(err, whatsmeow.ErrMediaDownloadFailedWith403)
}

// downloadAttachment decrypts a message's media into a temp file next to dest_path, then renames it over dest_path.
// An expired link is retried through the phone; the file is capped at 100 MiB and never follows a symlink.
func (s *server) downloadAttachment(ctx context.Context, req ipc.Request) ipc.Reply {
	var p downloadParams
	if e := decode(req, &p); e != nil {
		return failErr(req.ID, e)
	}
	chat, err := s.chatFor(ctx, p.ThreadID)
	if err != nil {
		return failErr(req.ID, err)
	}
	m, found, gerr := s.store.GetMessage(ctx, chat.JID, p.MessageID)
	if gerr != nil {
		return fail(req.ID, api.CodeIO, "message database error")
	}
	if !found || p.AttachmentID != normalize.AttachmentID || len(m.Raw) == 0 {
		return fail(req.ID, api.CodeMessageNotFound, "the message or attachment is not stored")
	}
	var raw waE2E.Message
	if proto.Unmarshal(m.Raw, &raw) != nil {
		return fail(req.ID, api.CodeMessageNotFound, "the message or attachment is not stored")
	}
	dl := pickDownloadable(&raw)
	if dl == nil || m.Unsent {
		return fail(req.ID, api.CodeMessageNotFound, "the message has no downloadable media")
	}
	dest, derr := openDestination(p.DestPath)
	if derr != nil {
		return failErr(req.ID, derr)
	}
	defer dest.close()
	if dl.length > maxDownloadBytes {
		return fail(req.ID, api.CodeIO, "attachment exceeds 100 MiB")
	}
	cl := s.cur()
	if !cl.HasDevice() {
		return fail(req.ID, api.CodeNotLoggedIn, "no WhatsApp account is linked")
	}
	if !cl.IsConnected() {
		return fail(req.ID, api.CodeNotConnected, "not connected to WhatsApp")
	}
	tmp, tmpName, terr := dest.newTemp()
	if terr != nil {
		return fail(req.ID, api.CodeIO, "could not write dest_path")
	}
	committed := false
	defer func() {
		if !committed {
			_ = tmp.Close()
			dest.discard(tmpName)
		}
	}()
	cf := &cappedFile{f: tmp, limit: maxDownloadBytes + downloadSlack}
	err = s.fetchMedia(ctx, cl, dl, cf)
	if isExpiredMedia(err) {
		err = s.retryDownload(ctx, cl, chat, m, &raw, dl, cf)
	}
	if err != nil {
		return failErr(req.ID, downloadError(err, cf))
	}
	size, sum, herr := hashFile(tmp)
	if herr != nil {
		return fail(req.ID, api.CodeIO, "could not write dest_path")
	}
	if size > maxDownloadBytes {
		return fail(req.ID, api.CodeIO, "attachment exceeds 100 MiB")
	}
	if tmp.Chmod(0o600) != nil || tmp.Sync() != nil || tmp.Close() != nil {
		return fail(req.ID, api.CodeIO, "could not write dest_path")
	}
	if dest.commit(tmpName) != nil {
		return fail(req.ID, api.CodeIO, "could not write dest_path")
	}
	committed = true
	return ok(req.ID, map[string]any{"path": p.DestPath, "mime": dl.mime, "bytes": size, "sha256": sum})
}

// fetchMedia empties cf and downloads the media into it.
func (s *server) fetchMedia(ctx context.Context, cl wa.Client, dl *downloadable, cf *cappedFile) error {
	if err := cf.rewind(); err != nil {
		return err
	}
	return cl.DownloadToFile(ctx, dl.msg, cf)
}

// downloadError maps a failed download to its protocol error; only *api.Error text and the size cap are told apart.
func downloadError(err error, cf *cappedFile) *api.Error {
	if cf.over.Load() || errors.Is(err, errTooLarge) {
		return api.Errorf(api.CodeIO, "attachment exceeds 100 MiB")
	}
	var e *api.Error
	if errors.As(err, &e) {
		return e
	}
	return api.Errorf(api.CodeIO, "media download failed")
}

// retryDownload asks the phone to re-upload expired media, then downloads it once more into cf.
func (s *server) retryDownload(ctx context.Context, cl wa.Client, chat store.Chat, m store.Msg, raw *waE2E.Message, dl *downloadable, cf *cappedFile) error {
	expired := api.Errorf(api.CodeAttachmentExpired, "the media has expired and the phone could not provide it again")
	chatWire, err1 := types.ParseJID(m.ChatWire)
	sender, err2 := types.ParseJID(m.SenderWire)
	if err1 != nil || err2 != nil {
		return expired
	}
	info := &types.MessageInfo{
		MessageSource: types.MessageSource{Chat: chatWire, Sender: sender, IsFromMe: m.FromMe, IsGroup: chat.Kind == api.KindGroup},
		ID:            m.ID,
	}
	if r := s.blockedReason(); r != "" {
		return api.Errorf(api.CodeAttachmentExpired, "the media has expired and cannot be requested from the phone while sending is blocked (%s)", r)
	}
	key := dl.msg.GetMediaKey()
	mkey := mediaKey(m.ChatWire, m.ID)
	ch := s.media.register(mkey)
	defer s.media.unregister(mkey, ch)
	if err := cl.SendMediaRetryReceipt(ctx, info, key); err != nil {
		return expired
	}
	timer := time.NewTimer(s.tm.mediaRetry)
	defer timer.Stop()
	var res *mediaRetryResult
	select {
	case res = <-ch:
	case <-timer.C:
		return expired
	case <-ctx.Done():
		return expired
	}
	notif, err := cl.DecryptMediaRetry(res.evt, key)
	if err != nil || notif.GetResult() != waMmsRetry.MediaRetryNotification_SUCCESS || notif.GetDirectPath() == "" {
		return expired
	}
	dl.setDir(notif.GetDirectPath())
	if nraw, merr := proto.Marshal(raw); merr == nil {
		_, _, _ = s.store.UpdateMessage(ctx, chat.JID, m.ID, func(x *store.Msg) { x.Raw = nraw })
	}
	if err := s.fetchMedia(ctx, cl, dl, cf); err != nil {
		if cf.over.Load() {
			return err
		}
		return expired
	}
	return nil
}
