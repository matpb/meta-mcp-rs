package server

import (
	"bytes"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/proto/waMmsRetry"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	"go.mau.fi/whatsmeow/util/cbcutil"
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
	for _, expired := range []error{whatsmeow.ErrMediaDownloadFailedWith404, whatsmeow.ErrMediaDownloadFailedWith410, whatsmeow.ErrMediaDownloadFailedWith403} {
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

// dirNames lists the entries of dir, so a test can see that no temp file was left behind.
func dirNames(t *testing.T, dir string) []string {
	t.Helper()
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	var out []string
	for _, e := range entries {
		out = append(out, e.Name())
	}
	return out
}

// Review W1: no size cap and a symlink at dest_path was followed. Now the download is refused and the victim stays intact.
func TestReviewDownloadNoCapAndSymlinkFollow(t *testing.T) {
	h, c := seededImage(t, true)
	big := make([]byte, 120<<20) // 120 MiB > the 100 MiB cap the docs promise
	c.downloadFn = func(whatsmeow.DownloadableMessage) ([]byte, error) { return big, nil }
	dir := t.TempDir()
	victim := filepath.Join(dir, "victim.txt")
	if err := os.WriteFile(victim, []byte("precious"), 0o600); err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(dir, "out.jpg")
	if err := os.Symlink(victim, link); err != nil {
		t.Fatal(err)
	}
	rep := h.call("download_attachment", dl(link))
	if rep.OK {
		fi, _ := os.Stat(victim)
		t.Fatalf("a symlink at dest_path was accepted (victim now %d bytes)", fi.Size())
	}
	if data, _ := os.ReadFile(victim); string(data) != "precious" {
		t.Fatalf("the symlink target was overwritten: %d bytes", len(data))
	}
	if got := dirNames(t, dir); fmt.Sprint(got) != "[out.jpg victim.txt]" {
		t.Fatalf("directory after the refusal: %v", got)
	}

	// the same payload to a plain path is cut off by the cap while it streams, whatever the sender advertised
	dest := filepath.Join(dir, "plain.jpg")
	rep = h.call("download_attachment", dl(dest))
	h.wantErr(rep, api.CodeIO)
	if !strings.Contains(rep.Error.Message, "100 MiB") {
		t.Fatalf("message %q", rep.Error.Message)
	}
	c.mu.Lock()
	written, werr := c.written, c.writeErr
	c.mu.Unlock()
	if !errors.Is(werr, errTooLarge) || written > maxDownloadBytes+downloadSlack {
		t.Fatalf("the cap must stop the stream: %d bytes accepted, write error %v", written, werr)
	}
	if got := dirNames(t, dir); fmt.Sprint(got) != "[out.jpg victim.txt]" {
		t.Fatalf("the cap left files behind: %v", got)
	}
}

func TestDownloadCapAllowsExactlyTheLimit(t *testing.T) {
	h, c := seededImage(t, true)
	big := make([]byte, maxDownloadBytes+1)
	for i := range big {
		big[i] = byte(i)
	}
	c.downloadFn = func(whatsmeow.DownloadableMessage) ([]byte, error) { return big[:maxDownloadBytes], nil }
	dir := t.TempDir()
	dest := filepath.Join(dir, "exact.bin")
	res := h.mustCall("download_attachment", dl(dest))
	sum := sha256.Sum256(big[:maxDownloadBytes])
	if res["bytes"] != float64(maxDownloadBytes) || res["sha256"] != hex.EncodeToString(sum[:]) {
		t.Fatalf("result %v", res)
	}
	if fi, err := os.Stat(dest); err != nil || fi.Size() != maxDownloadBytes {
		t.Fatalf("file %v %v", fi, err)
	}
	c.downloadFn = func(whatsmeow.DownloadableMessage) ([]byte, error) { return big, nil }
	other := filepath.Join(dir, "over.bin")
	h.wantErr(h.call("download_attachment", dl(other)), api.CodeIO)
	if got := dirNames(t, dir); fmt.Sprint(got) != "[exact.bin]" {
		t.Fatalf("one byte over the limit must leave nothing behind: %v", got)
	}
}

func TestDownloadRefusesSymlinkedDestinationAndDirectory(t *testing.T) {
	h, _ := seededImage(t, true)
	root := t.TempDir()
	real := filepath.Join(root, "real")
	if err := os.Mkdir(real, 0o700); err != nil {
		t.Fatal(err)
	}
	victim := filepath.Join(real, "victim.txt")
	_ = os.WriteFile(victim, []byte("precious"), 0o600)
	linkDir := filepath.Join(root, "linkdir")
	if err := os.Symlink(real, linkDir); err != nil {
		t.Fatal(err)
	}
	danglingTarget := filepath.Join(root, "nowhere")
	dangling := filepath.Join(real, "dangling.jpg")
	_ = os.Symlink(danglingTarget, dangling)
	subdir := filepath.Join(real, "subdir")
	_ = os.Mkdir(subdir, 0o700)

	for name, tc := range map[string]struct{ dest, msg string }{
		"symlinked parent":   {filepath.Join(linkDir, "out.jpg"), "symbolic link"},
		"symlink to a file":  {filepath.Join(real, "victim.txt-link"), "symbolic link"},
		"dangling symlink":   {dangling, "symbolic link"},
		"existing directory": {subdir, "regular file"},
	} {
		if name == "symlink to a file" {
			if err := os.Symlink(victim, tc.dest); err != nil {
				t.Fatal(err)
			}
		}
		rep := h.call("download_attachment", dl(tc.dest))
		h.wantErr(rep, api.CodeBadParams)
		if !strings.Contains(rep.Error.Message, tc.msg) {
			t.Fatalf("%s: message %q should name %q", name, rep.Error.Message, tc.msg)
		}
	}
	if data, _ := os.ReadFile(victim); string(data) != "precious" {
		t.Fatalf("victim overwritten: %q", data)
	}
	if _, err := os.Lstat(danglingTarget); err == nil {
		t.Fatal("a dangling symlink target was created")
	}
	if got := dirNames(t, real); fmt.Sprint(got) != "[dangling.jpg subdir victim.txt victim.txt-link]" {
		t.Fatalf("directory after the refusals: %v", got)
	}
}

func TestDownloadReplacesTheFileInsteadOfWritingThroughIt(t *testing.T) {
	h, _ := seededImage(t, true)
	dir := t.TempDir()
	victim := filepath.Join(dir, "victim.txt")
	_ = os.WriteFile(victim, []byte("precious"), 0o644)
	dest := filepath.Join(dir, "out.jpg")
	if err := os.Link(victim, dest); err != nil {
		t.Skipf("hard links unavailable: %v", err)
	}
	h.mustCall("download_attachment", dl(dest))
	if data, _ := os.ReadFile(victim); string(data) != "precious" {
		t.Fatalf("a hard link to another file was written through: %q", data)
	}
	if data, _ := os.ReadFile(dest); string(data) != "hello" {
		t.Fatalf("dest %q", data)
	}
	if fi, _ := os.Stat(dest); fi.Mode().Perm() != 0o600 {
		t.Fatalf("mode %v", fi.Mode().Perm())
	}
	if got := dirNames(t, dir); fmt.Sprint(got) != "[out.jpg victim.txt]" {
		t.Fatalf("a temp file was left behind: %v", got)
	}
}

func TestDownloadAdvertisedSizeOverTheCapIsRefusedUpFront(t *testing.T) {
	h, c := startPaired(t, true)
	huge := &waE2E.Message{VideoMessage: &waE2E.VideoMessage{
		Mimetype: ptr("video/mp4"), MediaKey: []byte{9, 9}, DirectPath: ptr("/old/path"), FileLength: ptr(uint64(maxDownloadBytes + 1)),
	}}
	if !c.deliver(msgEvent(peerPN, peerPN, false, "V1", 1700000100, huge)) {
		t.Fatal("refused")
	}
	c.downloadFn = func(whatsmeow.DownloadableMessage) ([]byte, error) { return mediaBytes, nil }
	dir := t.TempDir()
	rep := h.call("download_attachment", map[string]any{"thread_id": peerPN, "message_id": "V1", "attachment_id": "0", "dest_path": filepath.Join(dir, "v.mp4")})
	h.wantErr(rep, api.CodeIO)
	c.mu.Lock()
	n := len(c.downloads)
	c.mu.Unlock()
	if n != 0 || len(dirNames(t, dir)) != 0 {
		t.Fatalf("nothing may be fetched or written: downloads %d files %v", n, dirNames(t, dir))
	}
}

func TestDownloadFailureLeavesNoTempFile(t *testing.T) {
	h, c := seededImage(t, true)
	c.downloadFn = func(whatsmeow.DownloadableMessage) ([]byte, error) { return nil, errors.New("boom") }
	dir := t.TempDir()
	h.wantErr(h.call("download_attachment", dl(filepath.Join(dir, "out.jpg"))), api.CodeIO)
	if got := dirNames(t, dir); len(got) != 0 {
		t.Fatalf("files after a failed download: %v", got)
	}
}

func TestDownloadForbiddenIsRetriedThroughThePhone(t *testing.T) {
	h, c := seededImage(t, true)
	c.downloadFn = func(m whatsmeow.DownloadableMessage) ([]byte, error) {
		if m.GetDirectPath() == "/old/path" {
			return nil, fmt.Errorf("download: %w", whatsmeow.ErrMediaDownloadFailedWith403)
		}
		return mediaBytes, nil
	}
	c.decryptRet = retryNotification("/new/path", waMmsRetry.MediaRetryNotification_SUCCESS)
	dest := filepath.Join(t.TempDir(), "out.jpg")
	id := h.send("download_attachment", dl(dest))
	<-c.receipts
	c.deliver(&events.MediaRetry{MessageID: "I1", ChatID: mustJID(peerPN)})
	if res := h.mustOK(h.reply(id)); res["bytes"] != float64(5) {
		t.Fatalf("result %v", res)
	}
}

func TestCappedFileStopsEveryWritePath(t *testing.T) {
	newCapped := func(limit int64) (*cappedFile, string) {
		path := filepath.Join(t.TempDir(), "f")
		f, err := os.OpenFile(path, os.O_RDWR|os.O_CREATE|os.O_EXCL, 0o600)
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { f.Close() })
		return &cappedFile{f: f, limit: limit}, path
	}
	if _, bypass := any(&cappedFile{}).(io.ReaderFrom); bypass {
		t.Fatal("cappedFile must not expose ReadFrom, io.Copy would bypass Write")
	}
	if _, bypass := any(&cappedFile{}).(io.WriterTo); bypass {
		t.Fatal("cappedFile must not expose WriteTo")
	}

	cf, path := newCapped(1000)
	n, err := io.Copy(cf, io.TeeReader(bytes.NewReader(make([]byte, 1<<20)), io.Discard)) // the way whatsmeow streams a body
	if !errors.Is(err, errTooLarge) || !cf.over.Load() {
		t.Fatalf("io.Copy past the limit: n=%d err=%v over=%v", n, err, cf.over.Load())
	}
	if fi, _ := os.Stat(path); fi.Size() > 1000 {
		t.Fatalf("file grew to %d bytes past a limit of 1000", fi.Size())
	}

	cf, _ = newCapped(10)
	if _, err := cf.Write(make([]byte, 10)); err != nil {
		t.Fatalf("a write that ends at the limit: %v", err)
	}
	if _, err := cf.Write([]byte{1}); !errors.Is(err, errTooLarge) {
		t.Fatalf("one byte past: %v", err)
	}
	if _, err := cf.WriteAt([]byte{1, 2}, 9); !errors.Is(err, errTooLarge) {
		t.Fatalf("WriteAt across the limit: %v", err)
	}
	if _, err := cf.WriteAt([]byte{1}, -1); !errors.Is(err, errTooLarge) {
		t.Fatalf("WriteAt at a negative offset: %v", err)
	}
	if err := cf.Truncate(11); !errors.Is(err, errTooLarge) {
		t.Fatalf("Truncate past the limit: %v", err)
	}
	if err := cf.Truncate(4); err != nil {
		t.Fatalf("Truncate below the limit: %v", err)
	}
	if err := cf.rewind(); err != nil || cf.over.Load() {
		t.Fatalf("rewind: %v over=%v", err, cf.over.Load())
	}
	if _, err := cf.Write(make([]byte, 10)); err != nil {
		t.Fatalf("a rewound file takes the full limit again: %v", err)
	}
}

// whatsmeow decrypts a downloaded file in place with WriteAt and Truncate; the capped file must carry that through.
func TestCappedFileTakesTheLibrarysDecryptInPlace(t *testing.T) {
	plain := make([]byte, 300_001)
	if _, err := rand.Read(plain); err != nil {
		t.Fatal(err)
	}
	key, iv := make([]byte, 32), make([]byte, 16)
	_, _ = rand.Read(key)
	_, _ = rand.Read(iv)
	cipherText, err := cbcutil.Encrypt(key, iv, append([]byte(nil), plain...))
	if err != nil {
		t.Fatal(err)
	}
	open := func(limit int64) *cappedFile {
		f, err := os.OpenFile(filepath.Join(t.TempDir(), "f"), os.O_RDWR|os.O_CREATE|os.O_EXCL, 0o600)
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { f.Close() })
		return &cappedFile{f: f, limit: limit}
	}

	cf := open(int64(len(plain)) + downloadSlack)
	if _, err := io.Copy(cf, bytes.NewReader(cipherText)); err != nil {
		t.Fatalf("streaming the ciphertext: %v", err)
	}
	if _, err := cf.Seek(0, io.SeekStart); err != nil {
		t.Fatal(err)
	}
	if err := cbcutil.DecryptFile(key, iv, cf); err != nil {
		t.Fatalf("decrypting in place: %v", err)
	}
	size, sum, err := hashFile(cf.f)
	want := sha256.Sum256(plain)
	if err != nil || size != int64(len(plain)) || sum != hex.EncodeToString(want[:]) {
		t.Fatalf("decrypted file: %d bytes %s %v", size, sum, err)
	}

	small := open(int64(len(plain)))
	if _, err := io.Copy(small, bytes.NewReader(cipherText)); !errors.Is(err, errTooLarge) || !small.over.Load() {
		t.Fatalf("a ciphertext longer than the limit must be refused: %v", err)
	}
}
