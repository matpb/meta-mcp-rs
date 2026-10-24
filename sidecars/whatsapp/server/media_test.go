package server

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/proto/waMmsRetry"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	"google.golang.org/protobuf/proto"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
)

var mediaBytes = []byte("hello")

// seededImage stores an incoming image message "I1" and scripts a download that serves mediaBytes.
func seededImage(t *testing.T, connected bool, opts ...opt) (*harness, *fakeClient) {
	t.Helper()
	h, c := startPaired(t, connected, opts...)
	img := &waE2E.Message{ImageMessage: &waE2E.ImageMessage{
		Mimetype: ptr("image/jpeg"), MediaKey: []byte{9, 9}, DirectPath: ptr("/old/path"), FileLength: ptr(uint64(5)), Caption: ptr("pic"),
	}}
	if !c.deliver(msgEvent(peerPN, peerPN, false, "I1", 1700000100, img)) {
		t.Fatal("refused")
	}
	h.consume("thread")
	h.consume("message")
	c.downloadFn = func(whatsmeow.DownloadableMessage) ([]byte, error) { return mediaBytes, nil }
	return h, c
}

func dl(dest string) map[string]any {
	return map[string]any{"thread_id": peerPN, "message_id": "I1", "attachment_id": "0", "dest_path": dest}
}

func TestDownloadAttachmentWritesFile(t *testing.T) {
	h, _ := seededImage(t, true)
	dest := filepath.Join(t.TempDir(), "out.jpg")
	if err := os.WriteFile(dest, []byte("old content that is longer than the new one"), 0o644); err != nil {
		t.Fatal(err)
	}
	res := h.mustCall("download_attachment", dl(dest))
	sum := sha256.Sum256(mediaBytes)
	if res["path"] != dest || res["mime"] != "image/jpeg" || res["bytes"] != float64(5) || res["sha256"] != hex.EncodeToString(sum[:]) {
		t.Fatalf("result %v", res)
	}
	data, err := os.ReadFile(dest)
	if err != nil || string(data) != "hello" {
		t.Fatalf("file %q %v", data, err)
	}
	if fi, _ := os.Stat(dest); fi.Mode().Perm() != 0o600 {
		t.Fatalf("mode %v", fi.Mode().Perm())
	}
}

func TestDownloadAttachmentErrors(t *testing.T) {
	h, c := seededImage(t, true)
	h.incoming(c, peerPN, "T1", 1700000200, "just text")
	dest := filepath.Join(t.TempDir(), "out.jpg")
	req := func(k string, v any) map[string]any {
		m := dl(dest)
		m[k] = v
		return m
	}
	h.wantErr(h.call("download_attachment", req("attachment_id", "1")), api.CodeMessageNotFound)
	h.wantErr(h.call("download_attachment", req("message_id", "NOPE")), api.CodeMessageNotFound)
	h.wantErr(h.call("download_attachment", req("message_id", "T1")), api.CodeMessageNotFound)
	h.wantErr(h.call("download_attachment", req("thread_id", "999@s.whatsapp.net")), api.CodeThreadNotFound)
	h.wantErr(h.call("download_attachment", req("dest_path", "relative.jpg")), api.CodeBadParams)
	h.wantErr(h.call("download_attachment", req("dest_path", filepath.Join(dest, "missing", "x.jpg"))), api.CodeBadParams)
	c.downloadFn = func(whatsmeow.DownloadableMessage) ([]byte, error) { return nil, errors.New("boom") }
	h.wantErr(h.call("download_attachment", dl(dest)), api.CodeIO)
	if _, err := os.Stat(dest); err == nil {
		t.Fatal("a failed download must not leave a file")
	}
	c.mu.Lock()
	c.connected = false
	c.mu.Unlock()
	h.wantErr(h.call("download_attachment", dl(dest)), api.CodeNotConnected)
}

func retryNotification(path string, result waMmsRetry.MediaRetryNotification_ResultType) func(*events.MediaRetry, []byte) (*waMmsRetry.MediaRetryNotification, error) {
	return func(_ *events.MediaRetry, key []byte) (*waMmsRetry.MediaRetryNotification, error) {
		if string(key) != string([]byte{9, 9}) {
			return nil, errors.New("wrong media key")
		}
		return &waMmsRetry.MediaRetryNotification{DirectPath: ptr(path), Result: result.Enum()}, nil
	}
}

func expireOldPath(c *fakeClient) {
	c.downloadFn = func(m whatsmeow.DownloadableMessage) ([]byte, error) {
		if m.GetDirectPath() == "/old/path" {
			return nil, fmt.Errorf("download: %w", whatsmeow.ErrMediaDownloadFailedWith404)
		}
		return mediaBytes, nil
	}
}

func TestDownloadAttachmentRetriesThroughThePhone(t *testing.T) {
	for _, expired := range []error{whatsmeow.ErrMediaDownloadFailedWith404, whatsmeow.ErrMediaDownloadFailedWith410} {
		t.Run(fmt.Sprint(expired), func(t *testing.T) {
			h, c := seededImage(t, true)
			c.downloadFn = func(m whatsmeow.DownloadableMessage) ([]byte, error) {
				if m.GetDirectPath() == "/old/path" {
					return nil, fmt.Errorf("download: %w", expired)
				}
				return mediaBytes, nil
			}
			c.decryptRet = retryNotification("/new/path", waMmsRetry.MediaRetryNotification_SUCCESS)
			dest := filepath.Join(t.TempDir(), "out.jpg")
			id := h.send("download_attachment", dl(dest))
			info := <-c.receipts
			if info.ID != "I1" || info.Chat.String() != peerPN || info.Sender.String() != peerPN || info.IsFromMe || info.IsGroup {
				t.Fatalf("retry receipt info %+v", info)
			}
			if !c.deliver(&events.MediaRetry{MessageID: types.MessageID("I1"), ChatID: mustJID(peerPN)}) {
				t.Fatal("MediaRetry refused")
			}
			res := h.mustOK(h.reply(id))
			if res["bytes"] != float64(5) {
				t.Fatalf("result %v", res)
			}
			c.mu.Lock()
			downloads := append([]string(nil), c.downloads...)
			c.mu.Unlock()
			if fmt.Sprint(downloads) != "[/old/path /new/path]" {
				t.Fatalf("downloads %v", downloads)
			}
			m, _, _ := h.srv.store.GetMessage(h.srv.ctx, peerPN, "I1")
			var raw waE2E.Message
			if err := proto.Unmarshal(m.Raw, &raw); err != nil || raw.GetImageMessage().GetDirectPath() != "/new/path" {
				t.Fatalf("stored raw not updated: %v %v", err, raw.GetImageMessage().GetDirectPath())
			}
		})
	}
}

func TestDownloadAttachmentExpiredWhenPhoneDoesNotAnswer(t *testing.T) {
	h, c := seededImage(t, true, withTiming(func(*timing) {}))
	expireOldPath(c)
	h.wantErr(h.call("download_attachment", dl(filepath.Join(t.TempDir(), "out.jpg"))), api.CodeAttachmentExpired)
	if len(c.receipts) != 1 {
		t.Fatal("a retry receipt must be sent")
	}
	if len(h.srv.media.m) != 0 {
		t.Fatal("the waiter must be removed")
	}
}

func TestDownloadAttachmentExpiredVariants(t *testing.T) {
	cases := map[string]func(c *fakeClient){
		"phone says not found": func(c *fakeClient) {
			c.decryptRet = retryNotification("/new/path", waMmsRetry.MediaRetryNotification_NOT_FOUND)
		},
		"undecryptable answer": func(c *fakeClient) {
			c.decryptRet = func(*events.MediaRetry, []byte) (*waMmsRetry.MediaRetryNotification, error) {
				return nil, whatsmeow.ErrMediaNotAvailableOnPhone
			}
		},
		"still expired after retry": func(c *fakeClient) {
			c.decryptRet = retryNotification("/old/path", waMmsRetry.MediaRetryNotification_SUCCESS)
		},
	}
	for name, script := range cases {
		t.Run(name, func(t *testing.T) {
			h, c := seededImage(t, true)
			expireOldPath(c)
			script(c)
			id := h.send("download_attachment", dl(filepath.Join(t.TempDir(), "out.jpg")))
			<-c.receipts
			c.deliver(&events.MediaRetry{MessageID: "I1", ChatID: mustJID(peerPN)})
			h.wantErr(h.reply(id), api.CodeAttachmentExpired)
		})
	}
}

func TestMediaRetryForOtherMessagesIsIgnored(t *testing.T) {
	h, c := seededImage(t, true, withTiming(func(*timing) {}))
	expireOldPath(c)
	c.decryptRet = retryNotification("/new/path", waMmsRetry.MediaRetryNotification_SUCCESS)
	id := h.send("download_attachment", dl(filepath.Join(t.TempDir(), "out.jpg")))
	<-c.receipts
	if !c.deliver(&events.MediaRetry{MessageID: "OTHER", ChatID: mustJID(peerPN)}) {
		t.Fatal("refused")
	}
	h.wantErr(h.reply(id), api.CodeAttachmentExpired)
}

func TestDownloadBlockedSendsNoRetryReceipt(t *testing.T) {
	h, c := seededImage(t, true)
	expireOldPath(c)
	c.deliver(&events.StreamReplaced{})
	rep := h.call("download_attachment", dl(filepath.Join(t.TempDir(), "out.jpg")))
	h.wantErr(rep, api.CodeAttachmentExpired)
	if !strings.Contains(rep.Error.Message, "stream_replaced") {
		t.Fatalf("message must name the block: %q", rep.Error.Message)
	}
	if len(c.receipts) != 0 {
		t.Fatal("no retry receipt while sending is blocked")
	}
}

func TestConcurrentDownloadsShareTheRetryEvent(t *testing.T) {
	h, c := seededImage(t, true)
	expireOldPath(c)
	c.decryptRet = retryNotification("/new/path", waMmsRetry.MediaRetryNotification_SUCCESS)
	first := h.send("download_attachment", dl(filepath.Join(t.TempDir(), "a.jpg")))
	second := h.send("download_attachment", dl(filepath.Join(t.TempDir(), "b.jpg")))
	<-c.receipts
	<-c.receipts
	c.deliver(&events.MediaRetry{MessageID: "I1", ChatID: mustJID(peerPN)})
	for _, id := range []int{first, second} {
		if res := h.mustOK(h.reply(id)); res["bytes"] != float64(5) {
			t.Fatalf("result %v", res)
		}
	}
}

func TestHandlerRefusesEventsAfterClose(t *testing.T) {
	h, c := startPaired(t, true)
	h.srv.closed.Store(true)
	if c.deliver(&events.Connected{}) {
		t.Fatal("a closed server must not ack events")
	}
	h.srv.closed.Store(false)
}
