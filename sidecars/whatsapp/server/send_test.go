package server

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"image"
	"image/color"
	"image/jpeg"
	"image/png"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
)

// seededChat starts a connected linked server with one stored incoming DM message "A1".
func seededChat(t *testing.T) (*harness, *fakeClient) {
	t.Helper()
	h, c := startPaired(t, true)
	h.incoming(c, peerPN, "A1", 1700000100, "hello")
	h.consume("thread")
	h.consume("message")
	return h, c
}

func TestSendGateOrder(t *testing.T) {
	h := startServer(t, &fakeFactory{})
	h.mustHello()
	h.wantErr(h.call("send_text", map[string]any{"thread_id": peerPN, "text": "x"}), api.CodeNotLoggedIn)

	h, c := startPaired(t, false)
	h.wantErr(h.call("send_text", map[string]any{"thread_id": peerPN, "text": "x"}), api.CodeNotConnected)
	c.mu.Lock()
	c.connected = true
	c.mu.Unlock()
	rep := h.call("send_text", map[string]any{"thread_id": peerPN, "text": "x"})
	h.wantErr(rep, api.CodeThreadNotFound)
	if !strings.Contains(rep.Error.Message, "not registered") {
		t.Fatalf("message %q", rep.Error.Message)
	}
	rep = h.call("send_reaction", map[string]any{"thread_id": peerPN, "message_id": "A1", "emoji": "x"})
	h.wantErr(rep, api.CodeThreadNotFound)
	if !strings.Contains(rep.Error.Message, "open this chat on the phone") {
		t.Fatalf("message %q", rep.Error.Message)
	}
	if len(c.sentSnapshot()) != 0 {
		t.Fatal("nothing may be sent for an unknown chat")
	}
}

func TestSendTextStoresAndReportsMessage(t *testing.T) {
	h, c := seededChat(t)
	res := h.mustCall("send_text", map[string]any{"thread_id": peerPN, "text": "hi there"})
	if res["message_id"] != "3EB0FAKE0001" || res["ts"] != float64(1700000500) {
		t.Fatalf("result %v", res)
	}
	sent := c.sentSnapshot()
	if len(sent) != 1 || sent[0].to.String() != peerPN || sent[0].extra.ID != "3EB0FAKE0001" || sent[0].msg.GetConversation() != "hi there" || sent[0].extra.Peer {
		t.Fatalf("sent %+v", sent)
	}
	ev := h.event("message")
	m := ev["message"].(map[string]any)
	if ev["direction"] != "out" || m["id"] != "3EB0FAKE0001" || m["text"] != "hi there" || m["sender"].(map[string]any)["is_me"] != true || m["thread_id"] != peerPN {
		t.Fatalf("message event %v", ev)
	}
	msgs, _ := h.fetch(peerPN, 1)
	if msgs[0]["id"] != "3EB0FAKE0001" {
		t.Fatalf("stored %v", msgs)
	}
	if h.threads()[0]["last_message_preview"] != "hi there" {
		t.Fatal("preview not updated")
	}
}

func TestSendTextByAlternateThreadID(t *testing.T) {
	h, c := startPaired(t, true)
	evt := msgEvent(peerLID, peerLID, false, "A1", 1700000100, text("hello"))
	evt.Info.SenderAlt = mustJID(peerPN)
	c.deliver(evt)
	h.mustCall("send_text", map[string]any{"thread_id": peerLID, "text": "hi"})
	if sent := c.sentSnapshot(); sent[0].to.String() != peerPN {
		t.Fatalf("sent to %v", sent[0].to)
	}
}

func TestSendTextReply(t *testing.T) {
	h, c := seededChat(t)
	h.mustCall("send_text", map[string]any{"thread_id": peerPN, "text": "re: hello", "reply_to": "A1"})
	ext := c.sentSnapshot()[0].msg.GetExtendedTextMessage()
	ci := ext.GetContextInfo()
	if ext.GetText() != "re: hello" || ci.GetStanzaID() != "A1" || ci.GetParticipant() != peerPN || ci.GetQuotedMessage().GetConversation() != "hello" {
		t.Fatalf("reply message %+v", ext)
	}
	own := h.mustCall("send_text", map[string]any{"thread_id": peerPN, "text": "mine"})["message_id"].(string)
	h.mustCall("send_text", map[string]any{"thread_id": peerPN, "text": "reply to me", "reply_to": own})
	ci = c.sentSnapshot()[2].msg.GetExtendedTextMessage().GetContextInfo()
	if ci.GetStanzaID() != own || ci.GetParticipant() != selfPN {
		t.Fatalf("own reply context %+v", ci)
	}
	h.wantErr(h.call("send_text", map[string]any{"thread_id": peerPN, "text": "x", "reply_to": "NOPE"}), api.CodeMessageNotFound)
	if len(c.sentSnapshot()) != 3 {
		t.Fatal("a reply to an unknown message must not be sent")
	}
}

func TestSendTextBadParams(t *testing.T) {
	h, c := seededChat(t)
	h.wantErr(h.call("send_text", map[string]any{"thread_id": peerPN, "text": ""}), api.CodeBadParams)
	h.wantErr(h.call("send_text", map[string]any{"thread_id": peerPN, "text": strings.Repeat("a", 65537)}), api.CodeBadParams)
	h.wantErr(h.call("send_text", "nope"), api.CodeBadParams)
	h.mustCall("send_text", map[string]any{"thread_id": peerPN, "text": strings.Repeat("a", 65536)})
	if len(c.sentSnapshot()) != 1 {
		t.Fatal("only the valid text may be sent")
	}
}

func TestSendErrorMapping(t *testing.T) {
	cases := []struct {
		name string
		err  error
		code string
	}{
		{"timeout", whatsmeow.ErrMessageTimedOut, api.CodeSendUnknown},
		{"deadline", fmt.Errorf("wrapped: %w", context.DeadlineExceeded), api.CodeSendUnknown},
		{"not connected after the frame was written", whatsmeow.ErrNotConnected, api.CodeSendUnknown},
		{"disconnected while waiting", fmt.Errorf("send: %w", &whatsmeow.DisconnectedError{Action: "send"}), api.CodeSendUnknown},
		{"not logged in", whatsmeow.ErrNotLoggedIn, api.CodeNotLoggedIn},
		{"server 500", fmt.Errorf("%w 500", whatsmeow.ErrServerReturnedError), api.CodeSendFailed},
		{"other", errors.New("boom 14155550100@s.whatsapp.net secret text"), api.CodeSendFailed},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			h, c := seededChat(t)
			c.sendFn = func(types.JID, *waE2E.Message, whatsmeow.SendRequestExtra) (whatsmeow.SendResponse, error) {
				return whatsmeow.SendResponse{}, tc.err
			}
			rep := h.call("send_text", map[string]any{"thread_id": peerPN, "text": "x"})
			h.wantErr(rep, tc.code)
			if strings.Contains(rep.Error.Message, "boom") || strings.Contains(rep.Error.Message, "14155550100") {
				t.Fatalf("error message leaks: %q", rep.Error.Message)
			}
			if tc.code == api.CodeSendUnknown && !strings.Contains(rep.Error.Message, "3EB0FAKE0001") {
				t.Fatalf("send_unknown must carry the message id: %q", rep.Error.Message)
			}
			if got := h.pending("connection"); len(got) != 0 {
				t.Fatalf("no fatal event expected: %v", got)
			}
			if msgs, _ := h.fetch(peerPN, 1); msgs[0]["id"] != "A1" {
				t.Fatal("a failed send must not be stored")
			}
		})
	}
}

func TestServer463BlocksSending(t *testing.T) {
	h, c := seededChat(t)
	c.sendFn = func(types.JID, *waE2E.Message, whatsmeow.SendRequestExtra) (whatsmeow.SendResponse, error) {
		return whatsmeow.SendResponse{}, fmt.Errorf("%w 463", whatsmeow.ErrServerReturnedError)
	}
	h.wantErr(h.call("send_text", map[string]any{"thread_id": peerPN, "text": "x"}), api.CodeSendingBlocked)
	ev := h.event("connection")
	if ev["state"] != "fatal" || ev["reason"] != "reachout_timelock" || ev["action"] != "wait" {
		t.Fatalf("connection %v", ev)
	}
	h.wantErr(h.call("send_text", map[string]any{"thread_id": peerPN, "text": "y"}), api.CodeSendingBlocked)
	if len(c.sentSnapshot()) != 1 {
		t.Fatal("no send may reach WhatsApp once blocked")
	}
}

func writePNG(t *testing.T, w, hgt int) string {
	t.Helper()
	img := image.NewRGBA(image.Rect(0, 0, w, hgt))
	for y := 0; y < hgt; y++ {
		for x := 0; x < w; x++ {
			img.Set(x, y, color.RGBA{R: uint8(x), G: uint8(y), B: 200, A: 255})
		}
	}
	var buf bytes.Buffer
	if err := png.Encode(&buf, img); err != nil {
		t.Fatal(err)
	}
	return writeTemp(t, "pic.png", buf.Bytes())
}

func writeTemp(t *testing.T, name string, data []byte) string {
	t.Helper()
	p := filepath.Join(t.TempDir(), name)
	if err := os.WriteFile(p, data, 0o600); err != nil {
		t.Fatal(err)
	}
	return p
}

func scriptUpload(c *fakeClient) {
	c.uploadResp = whatsmeow.UploadResponse{
		URL: "https://mmg.example/file", DirectPath: "/v/t62/file", MediaKey: []byte{1, 2, 3},
		FileEncSHA256: []byte{4}, FileSHA256: []byte{5}, FileLength: 1234,
	}
}

func TestSendMediaImageWithThumbnail(t *testing.T) {
	h, c := seededChat(t)
	scriptUpload(c)
	path := writePNG(t, 300, 200)
	res := h.mustCall("send_media", map[string]any{"thread_id": peerPN, "path": path, "mime": "image/png", "kind": "image", "caption": "look"})
	if res["message_id"] == "" || res["ts"] == nil {
		t.Fatalf("result %v", res)
	}
	if len(c.uploads) != 1 || c.uploads[0] != whatsmeow.MediaImage {
		t.Fatalf("uploads %v", c.uploads)
	}
	im := c.sentSnapshot()[0].msg.GetImageMessage()
	if im == nil || im.GetCaption() != "look" || im.GetMimetype() != "image/png" || im.GetURL() != "https://mmg.example/file" || im.GetDirectPath() != "/v/t62/file" ||
		im.GetFileLength() != 1234 || im.GetWidth() != 300 || im.GetHeight() != 200 || len(im.GetMediaKey()) != 3 {
		t.Fatalf("image message %+v", im)
	}
	thumb, err := jpeg.Decode(bytes.NewReader(im.GetJPEGThumbnail()))
	if err != nil {
		t.Fatalf("thumbnail is not a JPEG: %v", err)
	}
	if b := thumb.Bounds(); b.Dx() != 100 || b.Dy() != 66 {
		t.Fatalf("thumbnail size %v", b)
	}
	m := h.event("message")["message"].(map[string]any)
	atts := m["attachments"].([]any)
	if m["kind"] != "media" || m["text"] != "look" || len(atts) != 1 || atts[0].(map[string]any)["kind"] != "image" {
		t.Fatalf("stored message %v", m)
	}
}

func TestSendMediaSmallImageAndUndecodableImage(t *testing.T) {
	h, c := seededChat(t)
	scriptUpload(c)
	h.mustCall("send_media", map[string]any{"thread_id": peerPN, "path": writePNG(t, 50, 40), "mime": "image/png", "kind": "image"})
	thumb, err := jpeg.Decode(bytes.NewReader(c.sentSnapshot()[0].msg.GetImageMessage().GetJPEGThumbnail()))
	if err != nil || thumb.Bounds().Dx() != 50 || thumb.Bounds().Dy() != 40 {
		t.Fatalf("small thumbnail %v %v", err, thumb)
	}
	h.mustCall("send_media", map[string]any{"thread_id": peerPN, "path": writeTemp(t, "bad.png", []byte("not an image")), "mime": "image/png", "kind": "image"})
	im := c.sentSnapshot()[1].msg.GetImageMessage()
	if im == nil || im.JPEGThumbnail != nil {
		t.Fatalf("undecodable images are sent without a thumbnail: %+v", im)
	}
}

func TestSendMediaAudioVideoAndFile(t *testing.T) {
	h, c := seededChat(t)
	scriptUpload(c)
	ogg := writeTemp(t, "voice.ogg", []byte("OggS"))
	h.mustCall("send_media", map[string]any{"thread_id": peerPN, "path": ogg, "mime": "audio/ogg; codecs=opus", "kind": "audio"})
	h.mustCall("send_media", map[string]any{"thread_id": peerPN, "path": ogg, "mime": "audio/mpeg", "kind": "audio"})
	h.mustCall("send_media", map[string]any{"thread_id": peerPN, "path": writeTemp(t, "v.mp4", []byte("mp4")), "mime": "video/mp4", "kind": "video", "caption": "clip"})
	h.mustCall("send_media", map[string]any{"thread_id": peerPN, "path": writeTemp(t, "r.pdf", []byte("%PDF")), "mime": "application/pdf", "kind": "file", "filename": "report.pdf", "caption": "cap"})
	h.mustCall("send_media", map[string]any{"thread_id": peerPN, "path": writeTemp(t, "plain.txt", []byte("hello")), "kind": "file"})
	sent := c.sentSnapshot()
	if !sent[0].msg.GetAudioMessage().GetPTT() || sent[1].msg.GetAudioMessage().GetPTT() {
		t.Fatal("only ogg opus audio is a voice note")
	}
	if v := sent[2].msg.GetVideoMessage(); v.GetCaption() != "clip" || v.GetMimetype() != "video/mp4" {
		t.Fatalf("video %+v", v)
	}
	if d := sent[3].msg.GetDocumentMessage(); d.GetFileName() != "report.pdf" || d.GetTitle() != "report.pdf" || d.GetCaption() != "cap" || d.GetMimetype() != "application/pdf" {
		t.Fatalf("document %+v", d)
	}
	if d := sent[4].msg.GetDocumentMessage(); d.GetFileName() != "plain.txt" || !strings.HasPrefix(d.GetMimetype(), "text/plain") {
		t.Fatalf("default document %+v", d)
	}
	want := []whatsmeow.MediaType{whatsmeow.MediaAudio, whatsmeow.MediaAudio, whatsmeow.MediaVideo, whatsmeow.MediaDocument, whatsmeow.MediaDocument}
	if fmt.Sprint(c.uploads) != fmt.Sprint(want) {
		t.Fatalf("uploads %v", c.uploads)
	}
}

func TestSendMediaBadParamsAndUploadFailure(t *testing.T) {
	h, c := seededChat(t)
	scriptUpload(c)
	dir := t.TempDir()
	big := filepath.Join(dir, "big.bin")
	f, _ := os.Create(big)
	_ = f.Truncate(100<<20 + 1)
	_ = f.Close()
	ok := writeTemp(t, "a.txt", []byte("x"))
	base := func(k string, v any) map[string]any {
		m := map[string]any{"thread_id": peerPN, "path": ok, "mime": "text/plain", "kind": "file"}
		m[k] = v
		return m
	}
	h.wantErr(h.call("send_media", base("path", "relative.txt")), api.CodeBadParams)
	h.wantErr(h.call("send_media", base("path", dir)), api.CodeBadParams)
	h.wantErr(h.call("send_media", base("path", filepath.Join(dir, "missing"))), api.CodeBadParams)
	h.wantErr(h.call("send_media", base("path", big)), api.CodeBadParams)
	h.wantErr(h.call("send_media", base("kind", "sticker")), api.CodeBadParams)
	if len(c.uploads) != 0 {
		t.Fatal("bad params must not upload")
	}
	c.uploadErr = errors.New("cdn exploded")
	rep := h.call("send_media", base("kind", "file"))
	h.wantErr(rep, api.CodeSendFailed)
	if strings.Contains(rep.Error.Message, "exploded") {
		t.Fatal("upload error text leaked")
	}
}

func TestSendReaction(t *testing.T) {
	h, c := seededChat(t)
	h.mustCall("send_reaction", map[string]any{"thread_id": peerPN, "message_id": "A1", "emoji": "👍"})
	c.mu.Lock()
	r := c.reactions[0]
	c.mu.Unlock()
	if r.chat.String() != peerPN || r.sender.String() != peerPN || r.id != "A1" || r.emoji != "👍" {
		t.Fatalf("BuildReaction args %+v", r)
	}
	if sent := c.sentSnapshot(); len(sent) != 1 || sent[0].msg.GetReactionMessage().GetText() != "👍" {
		t.Fatalf("sent %+v", sent)
	}
	msgs, _ := h.fetch(peerPN, 1)
	rs := msgs[0]["reactions"].([]any)
	if len(rs) != 1 || rs[0].(map[string]any)["sender_id"] != selfPN {
		t.Fatalf("reactions %v", rs)
	}
	h.mustCall("send_reaction", map[string]any{"thread_id": peerPN, "message_id": "A1", "emoji": ""})
	if msgs, _ = h.fetch(peerPN, 1); len(msgs[0]["reactions"].([]any)) != 0 {
		t.Fatal("empty emoji must remove the reaction")
	}
	own := h.mustCall("send_text", map[string]any{"thread_id": peerPN, "text": "mine"})["message_id"].(string)
	h.mustCall("send_reaction", map[string]any{"thread_id": peerPN, "message_id": own, "emoji": "🔥"})
	c.mu.Lock()
	last := c.reactions[len(c.reactions)-1]
	c.mu.Unlock()
	if last.sender.String() != selfPN {
		t.Fatalf("own message reaction sender %v", last.sender)
	}
	h.wantErr(h.call("send_reaction", map[string]any{"thread_id": peerPN, "message_id": "NOPE", "emoji": "x"}), api.CodeMessageNotFound)
}

func TestSendReactionInGroupUsesParticipant(t *testing.T) {
	h, c := startPaired(t, true)
	c.deliver(msgEvent(group, peerPN, false, "G1", 1700000100, text("hi")))
	h.mustCall("send_reaction", map[string]any{"thread_id": group, "message_id": "G1", "emoji": "👍"})
	c.mu.Lock()
	r := c.reactions[0]
	c.mu.Unlock()
	if r.chat.String() != group || r.sender.String() != peerPN {
		t.Fatalf("BuildReaction args %+v", r)
	}
}

func TestMarkReadGroupsBySenderInGroups(t *testing.T) {
	h, c := startPaired(t, true)
	other := "14155550101@s.whatsapp.net"
	c.deliver(msgEvent(group, peerPN, false, "G1", 1700000100, text("a")))
	c.deliver(msgEvent(group, other, false, "G2", 1700000200, text("b")))
	c.deliver(msgEvent(group, peerPN, false, "G3", 1700000300, text("c")))
	c.deliver(msgEvent(group, selfPN, true, "G4", 1700000400, text("mine")))
	h.consume("thread")
	h.mustCall("mark_read", map[string]any{"thread_id": group})
	c.mu.Lock()
	marks := append([]markCall(nil), c.marks...)
	c.mu.Unlock()
	if len(marks) != 2 {
		t.Fatalf("marks %+v", marks)
	}
	for _, m := range marks {
		if m.chat.String() != group {
			t.Fatalf("chat %v", m.chat)
		}
		switch m.sender.String() {
		case peerPN:
			if fmt.Sprint(m.ids) != "[G3 G1]" {
				t.Fatalf("ids %v", m.ids)
			}
		case other:
			if fmt.Sprint(m.ids) != "[G2]" {
				t.Fatalf("ids %v", m.ids)
			}
		default:
			t.Fatalf("sender %v", m.sender)
		}
	}
	th := h.event("thread")["thread"].(map[string]any)
	if th["unread"] != false {
		t.Fatalf("thread %v", th)
	}
	h.mustCall("mark_read", map[string]any{"thread_id": group})
	c.mu.Lock()
	n := len(c.marks)
	c.mu.Unlock()
	if n != 2 {
		t.Fatal("nothing left to mark")
	}
}

func TestMarkReadDMAndUpTo(t *testing.T) {
	h, c := startPaired(t, true)
	for i, ts := range []int64{1700000100, 1700000200, 1700000300} {
		h.incoming(c, peerPN, fmt.Sprintf("D%d", i), ts, "x")
	}
	h.consume("thread")
	h.mustCall("mark_read", map[string]any{"thread_id": peerPN, "up_to_ts_ms": 1700000200 * 1000})
	c.mu.Lock()
	marks := append([]markCall(nil), c.marks...)
	c.mu.Unlock()
	if len(marks) != 1 || fmt.Sprint(marks[0].ids) != "[D1 D0]" || !marks[0].sender.IsEmpty() || marks[0].chat.String() != peerPN {
		t.Fatalf("marks %+v", marks)
	}
	if chat, _, _ := h.srv.store.GetChat(context.Background(), peerPN); chat.UnreadCount != 1 || chat.ReadUpToTS != 1700000200 {
		t.Fatalf("chat %+v", chat)
	}
	h.mustCall("mark_read", map[string]any{"thread_id": peerPN})
	c.mu.Lock()
	marks = append([]markCall(nil), c.marks...)
	c.mu.Unlock()
	if len(marks) != 2 || fmt.Sprint(marks[1].ids) != "[D2]" {
		t.Fatalf("marks %+v", marks)
	}
	if h.threads()[0]["unread"] != false {
		t.Fatal("thread should be read")
	}
}

func TestSetTyping(t *testing.T) {
	h, c := seededChat(t)
	h.mustCall("set_typing", map[string]any{"thread_id": peerPN, "on": true})
	h.mustCall("set_typing", map[string]any{"thread_id": peerPN, "on": false})
	c.mu.Lock()
	defer c.mu.Unlock()
	if len(c.chatPres) != 2 || c.chatPres[0].state != types.ChatPresenceComposing || c.chatPres[1].state != types.ChatPresencePaused || c.chatPres[0].jid.String() != peerPN {
		t.Fatalf("chat presences %+v", c.chatPres)
	}
}

func TestNotConnectedWithoutMessageIDMapsToNotConnected(t *testing.T) {
	h, c := seededChat(t)
	c.sendFn = func(types.JID, *waE2E.Message, whatsmeow.SendRequestExtra) (whatsmeow.SendResponse, error) {
		return whatsmeow.SendResponse{}, whatsmeow.ErrNotConnected
	}
	h.wantErr(h.call("send_reaction", map[string]any{"thread_id": peerPN, "message_id": "A1", "emoji": "x"}), api.CodeNotConnected)
	c.sendFn = func(types.JID, *waE2E.Message, whatsmeow.SendRequestExtra) (whatsmeow.SendResponse, error) {
		return whatsmeow.SendResponse{}, &whatsmeow.DisconnectedError{Action: "send"}
	}
	h.wantErr(h.call("send_reaction", map[string]any{"thread_id": peerPN, "message_id": "A1", "emoji": "x"}), api.CodeSendUnknown)
}

func TestMarkReadWalksAllUnreadInBatches(t *testing.T) {
	h, c := startPaired(t, true)
	for i := 0; i < 250; i++ {
		ts := int64(1700000000 + i/2)
		h.incoming(c, peerPN, fmt.Sprintf("U%03d", i), ts, "x")
	}
	h.consume("thread")
	h.consume("message")
	h.mustCall("mark_read", map[string]any{"thread_id": peerPN})
	c.mu.Lock()
	marks := append([]markCall(nil), c.marks...)
	c.mu.Unlock()
	seen := map[types.MessageID]bool{}
	for _, m := range marks {
		for _, id := range m.ids {
			seen[id] = true
		}
	}
	if len(seen) != 250 {
		t.Fatalf("marked %d distinct messages in %d calls", len(seen), len(marks))
	}
	chat, _, _ := h.srv.store.GetChat(context.Background(), peerPN)
	if chat.UnreadCount != 0 || chat.ReadUpToTS != 1700000124 {
		t.Fatalf("chat %+v", chat)
	}
}

func TestMarkReadFailureKeepsUnreadState(t *testing.T) {
	h, c := seededChat(t)
	c.mu.Lock()
	c.markErr = whatsmeow.ErrNotConnected
	c.mu.Unlock()
	h.wantErr(h.call("mark_read", map[string]any{"thread_id": peerPN}), api.CodeNotConnected)
	if chat, _, _ := h.srv.store.GetChat(context.Background(), peerPN); chat.UnreadCount != 1 || chat.ReadUpToTS != 0 {
		t.Fatalf("chat %+v", chat)
	}
}

// Review W5: a group read marks one receipt batch per participant, and a message in the same second as the mark is still unread.
func TestReviewMarkReadGroupSemantics(t *testing.T) {
	h, c := startPaired(t, true, fastTiming)
	for i, m := range []struct{ sender, id string }{{aliceGrpPN, "G1"}, {malloryPN, "G2"}, {aliceGrpPN, "G3"}} {
		if !c.deliver(groupMsg(m.sender, m.id, int64(1700000100+i*10), text("t"+m.id))) {
			t.Fatal("refused")
		}
	}
	h.consume("thread")
	h.consume("message")
	h.mustCall("mark_read", map[string]any{"thread_id": group})
	c.mu.Lock()
	marks := append([]markCall(nil), c.marks...)
	c.mu.Unlock()
	if len(marks) != 2 {
		t.Errorf("want one receipt batch per participant, got %d", len(marks))
	}
	for _, m := range marks {
		if m.chat.String() != group || (m.sender.String() != aliceGrpPN && m.sender.String() != malloryPN) {
			t.Errorf("receipt for %v from %v", m.chat, m.sender)
		}
	}
	// second call: nothing unread is left, nothing is sent
	h.mustCall("mark_read", map[string]any{"thread_id": group})
	c.mu.Lock()
	n := len(c.marks)
	c.mu.Unlock()
	if n != len(marks) {
		t.Errorf("a repeated mark_read re-sent receipts (%d -> %d)", len(marks), n)
	}
	// a new message in the same second as the newest read message is unread and gets its receipt
	if !c.deliver(groupMsg(aliceGrpPN, "G4", 1700000120, text("same second as the newest read message"))) {
		t.Fatal("refused")
	}
	if th := h.threads(); th[0]["unread"] != true {
		t.Errorf("a message in the same second as the mark must make the thread unread: %v", th[0])
	}
	h.mustCall("mark_read", map[string]any{"thread_id": group})
	c.mu.Lock()
	after := append([]markCall(nil), c.marks...)
	c.mu.Unlock()
	if len(after) != n+1 || fmt.Sprint(after[n].ids) != "[G4]" {
		t.Errorf("G4 must be marked read: %+v", after[n:])
	}
	if chat, _, _ := h.srv.store.GetChat(context.Background(), group); chat.UnreadCount != 0 || chat.ReadUpToTS != 1700000120 || chat.ReadUpToID != "G4" {
		t.Errorf("chat after the second mark %+v", chat)
	}
}

// Review W5: more than 100 unread messages sharing one timestamp were cut at 100.
func TestReviewMarkReadManyMessagesSameSecond(t *testing.T) {
	h, c := startPaired(t, true, fastTiming)
	for i := 0; i < 250; i++ {
		if !c.deliver(msgEvent(peerPN, peerPN, false, fmt.Sprintf("S%04d", i), 1700000100, text("burst"))) {
			t.Fatal("refused")
		}
	}
	h.consume("thread")
	h.consume("message")
	h.mustCall("mark_read", map[string]any{"thread_id": peerPN})
	c.mu.Lock()
	total := 0
	seen := map[types.MessageID]bool{}
	for _, m := range c.marks {
		total += len(m.ids)
		for _, id := range m.ids {
			seen[id] = true
		}
		if len(m.ids) > 100 {
			t.Errorf("a receipt carries %d ids, at most 100 are sent at a time", len(m.ids))
		}
	}
	calls := len(c.marks)
	c.mu.Unlock()
	if total != 250 || len(seen) != 250 || calls != 3 {
		t.Errorf("receipts for %d ids (%d distinct) in %d calls, want 250 in 3", total, len(seen), calls)
	}
	thr := h.threads()
	if thr[0]["unread"] != false {
		t.Errorf("unread flag after mark_read: %v", thr[0]["unread"])
	}
	chat, _, _ := h.srv.store.GetChat(context.Background(), peerPN)
	if chat.ReadUpToTS != 1700000100 || chat.ReadUpToID != "S0249" {
		t.Errorf("read mark %d %q", chat.ReadUpToTS, chat.ReadUpToID)
	}
	h.mustCall("mark_read", map[string]any{"thread_id": peerPN})
	c.mu.Lock()
	defer c.mu.Unlock()
	if len(c.marks) != calls {
		t.Errorf("a second mark_read re-sent receipts")
	}
}

// Review: the send gate answers thread_not_found for unknown chats and sending_blocked on all five ops after a ban.
func TestReviewSendGate(t *testing.T) {
	h, c := startPaired(t, true, fastTiming)
	h.wantErr(h.call("send_text", map[string]any{"thread_id": "15550009999@s.whatsapp.net", "text": "hi"}), "thread_not_found")
	h.wantErr(h.call("send_text", map[string]any{"thread_id": group, "text": "hi"}), "thread_not_found")
	h.incoming(c, peerPN, "P0", 1700000050, "hi")
	h.mustCall("send_text", map[string]any{"thread_id": peerPN, "text": "ok"})
	// the alias form resolves only once the mapping is known
	h.wantErr(h.call("send_text", map[string]any{"thread_id": peerLID, "text": "ok"}), "thread_not_found")
	if !c.deliver(&events.TemporaryBan{Code: 101, Expire: time.Hour}) {
		t.Fatal("refused")
	}
	for _, op := range []string{"send_text", "send_media", "send_reaction", "mark_read", "set_typing"} {
		rep := h.call(op, map[string]any{"thread_id": peerPN, "text": "x", "path": "/etc/hostname", "mime": "text/plain", "kind": "file", "message_id": "P0", "emoji": "x", "on": true})
		h.wantErr(rep, "sending_blocked")
	}
}

// Review: the phone normalisation the pair code flow depends on.
func TestReviewNormalizePhone(t *testing.T) {
	for in, want := range map[string]string{
		"+1 (415) 555-0100":    "14155550100",
		"14155550100":          "14155550100",
		"+14155550100":         "14155550100",
		"1234567":              "1234567",
		"123456789012345":      "123456789012345",
		"0141555501":           "",
		"123456":               "",
		"1234567890123456":     "",
		"+٣٣١٢٣٤٥٦٧٨٩":         "",
		"1 415 555 0100 ext 2": "",
		"":                     "",
		"+":                    "",
	} {
		got, ok := normalizePhone(in)
		if got != want || ok != (want != "") {
			t.Errorf("normalizePhone(%q) = %q, %v; want %q", in, got, ok, want)
		}
	}
}

// The owner's own number is accepted as a target before any chat exists; any other unknown number is not.
func TestSendTextToOwnNumberCreatesTheSelfChat(t *testing.T) {
	h, c := startPaired(t, true)
	h.wantErr(h.call("send_text", map[string]any{"thread_id": selfPN, "text": "x"}), api.CodeThreadNotFound)
	_ = h.srv.store.SetMeta(h.srv.ctx, metaOwnerJID, selfPN)
	h.wantErr(h.call("send_text", map[string]any{"thread_id": "15550009999@s.whatsapp.net", "text": "x"}), api.CodeThreadNotFound)
	res := h.mustCall("send_text", map[string]any{"thread_id": selfPN, "text": "note to self"})
	if res["message_id"] == "" || len(c.sentSnapshot()) != 1 {
		t.Fatalf("result %v, sent %d", res, len(c.sentSnapshot()))
	}
	chat, found, err := h.srv.store.GetChat(h.srv.ctx, selfPN)
	if err != nil || !found || chat.Kind != api.KindDM {
		t.Fatalf("self chat not stored: found=%v kind=%q err=%v", found, chat.Kind, err)
	}
}

// A send to an unknown phone number opens the chat once WhatsApp confirms the number; unregistered numbers,
// non-phone ids and the reaction, read and typing ops stay thread_not_found.
func TestSendTextOpensANewChatForARegisteredNumber(t *testing.T) {
	h, c := startPaired(t, true)
	c.mu.Lock()
	c.onWA = map[string]bool{"15550009999": true}
	c.mu.Unlock()
	rep := h.call("send_text", map[string]any{"thread_id": "15550001111@s.whatsapp.net", "text": "x"})
	h.wantErr(rep, api.CodeThreadNotFound)
	if !strings.Contains(rep.Error.Message, "not registered") {
		t.Fatalf("message %q", rep.Error.Message)
	}
	h.wantErr(h.call("send_text", map[string]any{"thread_id": "123@g.us", "text": "x"}), api.CodeThreadNotFound)
	h.wantErr(h.call("send_text", map[string]any{"thread_id": "abc@s.whatsapp.net", "text": "x"}), api.CodeThreadNotFound)
	h.wantErr(h.call("send_reaction", map[string]any{"thread_id": "15550009999@s.whatsapp.net", "message_id": "A1", "emoji": "x"}), api.CodeThreadNotFound)
	if len(c.sentSnapshot()) != 0 {
		t.Fatal("nothing may be sent before the number is confirmed")
	}
	res := h.mustCall("send_text", map[string]any{"thread_id": "15550009999@s.whatsapp.net", "text": "hello there"})
	if res["message_id"] == "" || len(c.sentSnapshot()) != 1 {
		t.Fatalf("result %v, sent %d", res, len(c.sentSnapshot()))
	}
	chat, found, err := h.srv.store.GetChat(h.srv.ctx, "15550009999@s.whatsapp.net")
	if err != nil || !found || chat.Kind != api.KindDM {
		t.Fatalf("new chat not stored: found=%v kind=%q err=%v", found, chat.Kind, err)
	}
	c.mu.Lock()
	queries := len(c.onWAQuery)
	c.mu.Unlock()
	h.mustCall("send_text", map[string]any{"thread_id": "15550009999@s.whatsapp.net", "text": "again"})
	c.mu.Lock()
	defer c.mu.Unlock()
	if len(c.onWAQuery) != queries {
		t.Fatal("a known chat must not be re-checked with WhatsApp")
	}
}
