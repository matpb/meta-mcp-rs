package engine

import (
	"context"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/rs/zerolog"
	"go.mau.fi/mautrix-meta/pkg/messagix/cookies"
	"go.mau.fi/mautrix-meta/pkg/messagix/socket"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

// openIn opens a session on a given data dir, so a second session can play the restart.
func openIn(t *testing.T, fc *fakeClient, dir string) (*session, *recorder) {
	t.Helper()
	rec := &recorder{}
	f := &factory{log: zerolog.Nop(), newClient: func(c *cookies.Cookies, _ zerolog.Logger) metaClient {
		if fc.jar == nil {
			fc.jar = c
		}
		return fc
	}}
	sess, err := f.Open(testState(), dir, rec.sink)
	if err != nil {
		t.Fatal(err)
	}
	s := sess.(*session)
	t.Cleanup(s.Close)
	return s, rec
}

func attachmentTable(thread int64, mid string, ts int64, url string) *table.LSTable {
	return &table.LSTable{
		LSInsertMessage: []*table.LSInsertMessage{{MessageId: mid, ThreadKey: thread, TimestampMs: ts, SenderId: alice}},
		LSInsertBlobAttachment: []*table.LSInsertBlobAttachment{{
			MessageId: mid, ThreadKey: thread, AttachmentFbid: "fbid1", AttachmentType: table.AttachmentTypeImage,
			Filename: "p.png", Filesize: 5, PlayableUrl: url, PlayableUrlMimeType: "image/png",
		}},
	}
}

func downloadOf(s *session, dest string) (api.DownloadResult, error) {
	return s.DownloadAttachment(context.Background(), api.DownloadParams{ThreadID: "222", MessageID: "fb:mid.$a1", AttachmentID: "fbid1", DestPath: dest})
}

// L4: download_attachment finds the message after a restart, from the persistent index, even before the thread is listed.
func TestDownloadAttachmentWorksAfterARestart(t *testing.T) {
	dir := t.TempDir()
	first, _ := openIn(t, newFakeClient(nil), dir)
	first.processTable(threadsTable(), modeInitial)
	first.processTable(attachmentTable(alice, "mid.$a1", 5000, "https://cdn.example/a.png"), modeLive)
	first.Close()

	fc := newFakeClient(nil)
	second, _ := openIn(t, fc, dir)
	if _, _, ok := second.cachedMessage("222", "fb:mid.$a1"); ok {
		t.Fatal("a fresh session must start with an empty cache")
	}
	urls := stubDownload(t, func(string) (io.ReadCloser, error) { return io.NopCloser(strings.NewReader("hello")), nil })
	dest := filepath.Join(t.TempDir(), "a.png")
	res, err := downloadOf(second, dest)
	if err != nil {
		t.Fatal(err)
	}
	if res.Bytes != 5 || res.Mime != "image/png" || len(*urls) != 1 || (*urls)[0] != "https://cdn.example/a.png" {
		t.Fatalf("result %+v urls %v", res, *urls)
	}

	for name, p := range map[string]api.DownloadParams{
		"another thread":  {ThreadID: "223", MessageID: "fb:mid.$a1", AttachmentID: "fbid1", DestPath: dest},
		"unknown message": {ThreadID: "222", MessageID: "fb:mid.$none", AttachmentID: "fbid1", DestPath: dest},
	} {
		if _, err := second.DownloadAttachment(t.Context(), p); err == nil || api.AsError(err).Code != api.CodeThreadNotFound {
			t.Fatalf("%s: %v, want thread_not_found", name, err)
		}
	}
	if _, err := second.DownloadAttachment(t.Context(), api.DownloadParams{ThreadID: "222", MessageID: "fb:mid.$a1", AttachmentID: "other", DestPath: dest}); err == nil || api.AsError(err).Code != api.CodeBadParams {
		t.Fatalf("unknown attachment: %v", err)
	}
}

func TestDownloadAttachmentRefreshesAnIndexedLinkThroughAFetch(t *testing.T) {
	dir := t.TempDir()
	soon := "https://cdn.example/old.png?oe=" + strings.ToUpper(strconv.FormatInt(time.Now().Unix()+5, 16))
	first, _ := openIn(t, newFakeClient(nil), dir)
	first.processTable(attachmentTable(alice, "mid.$a1", 5000, soon), modeLive)
	first.Close()

	fc := newFakeClient(nil)
	second, _ := openIn(t, fc, dir)
	if _, err := second.Validate(t.Context()); err != nil {
		t.Fatal(err)
	}
	fc.mu.Lock()
	fc.connected = true
	fc.mu.Unlock()
	refreshes := 0
	fc.exec = func(_ context.Context, tasks ...socket.Task) (*table.LSTable, error) {
		if _, ok := tasks[0].(*socket.FetchMessagesTask); !ok {
			t.Errorf("unexpected task %T", tasks[0])
		}
		refreshes++
		return attachmentTable(alice, "mid.$a1", 5000, "https://cdn.example/new.png"), nil
	}
	urls := stubDownload(t, func(string) (io.ReadCloser, error) { return io.NopCloser(strings.NewReader("fresh")), nil })
	// the thread is not listed yet, so the stored record vouches for it
	if _, err := downloadOf(second, filepath.Join(t.TempDir(), "a")); err != nil {
		t.Fatal(err)
	}
	if refreshes != 1 || len(*urls) != 1 || (*urls)[0] != "https://cdn.example/new.png" {
		t.Fatalf("refreshes %d urls %v", refreshes, *urls)
	}
	row, found, _ := second.e2eeStore().FBAttachments(t.Context(), "fb:mid.$a1")
	if !found || *row.Attachments[0].URL != "https://cdn.example/new.png" {
		t.Fatalf("the refreshed link must replace the stored one: %+v", row)
	}
}

func TestAttachmentIndexKeepsOnlyDownloadableMedia(t *testing.T) {
	s, _ := openTest(t, newFakeClient(nil))
	s.processTable(attachmentTable(alice, "mid.$a1", 5000, "https://cdn.example/a.png"), modeLive)
	s.processTable(&table.LSTable{
		LSInsertMessage: []*table.LSInsertMessage{{MessageId: "mid.$plain", ThreadKey: alice, TimestampMs: 6000, SenderId: alice, Text: "no media"}},
	}, modeLive)
	st := s.e2eeStore()
	if st == nil {
		t.Fatal("indexing must open the store")
	}
	if _, found, _ := st.FBAttachments(t.Context(), "fb:mid.$a1"); !found {
		t.Fatal("a downloadable attachment must be indexed")
	}
	if _, found, _ := st.FBAttachments(t.Context(), "fb:mid.$plain"); found {
		t.Fatal("a message without media must not be indexed")
	}
	if _, found, _ := st.FBAttachments(t.Context(), "fb:mid.$none"); found {
		t.Fatal("unknown message indexed")
	}
	if rows := attachmentRows([]api.Attachment{{ID: "l", Kind: api.AttLink}}); rows != 0 {
		t.Fatalf("link attachments are not downloadable, got %d rows", rows)
	}
}

func attachmentRows(atts []api.Attachment) int {
	u := "https://cdn.example/l"
	for i := range atts {
		atts[i].URL = &u
	}
	_, ok := attachmentRow(api.Message{ID: "fb:x", ThreadID: "222", Attachments: atts}, 1)
	if ok {
		return 1
	}
	return 0
}

func TestUnsentMessageDropsItsIndexedAttachment(t *testing.T) {
	s, _ := openTest(t, newFakeClient(nil))
	s.processTable(attachmentTable(alice, "mid.$a1", 5000, "https://cdn.example/a.png"), modeLive)
	st := s.e2eeStore()
	s.applyRemoval(api.Removal{ThreadID: "222", MessageID: "fb:mid.$a1", Unsent: true})
	if _, found, _ := st.FBAttachments(t.Context(), "fb:mid.$a1"); found {
		t.Fatal("an unsent message must lose its download record")
	}
	s.processTable(attachmentTable(alice, "mid.$a2", 6000, "https://cdn.example/b.png"), modeLive)
	s.applyRemoval(api.Removal{ThreadID: "222", MessageID: "fb:mid.$a2"})
	if _, found, _ := st.FBAttachments(t.Context(), "fb:mid.$a2"); found {
		t.Fatal("a deleted message must lose its download record")
	}
}

// Shared download rules: no symlink destination, no symlinked directory, size checked before any byte is fetched.
func TestDownloadRefusesSymlinkedDestinations(t *testing.T) {
	s, _ := sendSession(t)
	cachedAttachmentMsg(s, "https://cdn.example/a.png", nil)
	urls := stubDownload(t, func(string) (io.ReadCloser, error) { return io.NopCloser(strings.NewReader("hello")), nil })
	root := t.TempDir()
	victim := filepath.Join(root, "victim.txt")
	if err := os.WriteFile(victim, []byte("precious"), 0o600); err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(root, "dest.png")
	if err := os.Symlink(victim, link); err != nil {
		t.Fatal(err)
	}
	real := filepath.Join(root, "real")
	if err := os.Mkdir(real, 0o700); err != nil {
		t.Fatal(err)
	}
	linkedDir := filepath.Join(root, "linked")
	if err := os.Symlink(real, linkedDir); err != nil {
		t.Fatal(err)
	}
	p := api.DownloadParams{ThreadID: "222", MessageID: "fb:mid.$att", AttachmentID: "0"}

	p.DestPath = link
	_, err := s.DownloadAttachment(t.Context(), p)
	wantCode(t, err, api.CodeBadParams)
	if b, _ := os.ReadFile(victim); string(b) != "precious" {
		t.Fatalf("the symlink target was overwritten: %q", b)
	}

	p.DestPath = filepath.Join(linkedDir, "x.png")
	_, err = s.DownloadAttachment(t.Context(), p)
	wantCode(t, err, api.CodeBadParams)
	if names := entries(t, real); len(names) != 0 {
		t.Fatalf("files written through a symlinked directory: %v", names)
	}
	if len(*urls) != 0 {
		t.Fatalf("%d downloads started before the destination was refused", len(*urls))
	}

	p.DestPath = filepath.Join(real, "ok.png")
	if _, err := s.DownloadAttachment(t.Context(), p); err != nil {
		t.Fatalf("a plain destination must work: %v", err)
	}
	if b, _ := os.ReadFile(p.DestPath); string(b) != "hello" {
		t.Fatalf("content %q", b)
	}
}

// The check is repeated right before the rename, so a swap during the download is caught.
func TestCommitRefusesADestinationSwappedForASymlink(t *testing.T) {
	dir := t.TempDir()
	dest := filepath.Join(dir, "out.bin")
	tf, err := newTempFile(dest)
	if err != nil {
		t.Fatal(err)
	}
	defer tf.discard()
	victim := filepath.Join(dir, "victim")
	if err := os.WriteFile(victim, []byte("precious"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(victim, dest); err != nil {
		t.Fatal(err)
	}
	err = tf.commit()
	var ae *api.Error
	if !errors.As(err, &ae) || ae.Code != api.CodeBadParams {
		t.Fatalf("commit over a symlink: %v", err)
	}
	if b, _ := os.ReadFile(victim); string(b) != "precious" {
		t.Fatalf("victim overwritten: %q", b)
	}
	if fi, lerr := os.Lstat(dest); lerr != nil || fi.Mode()&os.ModeSymlink == 0 {
		t.Fatalf("a refused commit must leave the destination as it was: %v %v", fi, lerr)
	}
}

func TestDownloadRefusesAnAdvertisedSizeOverTheCap(t *testing.T) {
	s, _ := sendSession(t)
	mime, url, size := "image/png", "https://cdn.example/huge", int64(maxDownloadBytes+1)
	s.hist.put(api.Message{
		ID: "fb:mid.$huge", ThreadID: "222", Kind: api.MsgMedia, Reactions: []api.Reaction{},
		Attachments: []api.Attachment{{ID: "0", Kind: api.AttImage, Mime: &mime, URL: &url, Size: &size}},
	}, 5_000, true)
	urls := stubDownload(t, func(string) (io.ReadCloser, error) { return nil, errors.New("must not be called") })
	dest := filepath.Join(t.TempDir(), "a")
	_, err := s.DownloadAttachment(t.Context(), api.DownloadParams{ThreadID: "222", MessageID: "fb:mid.$huge", AttachmentID: "0", DestPath: dest})
	if ae := wantCode(t, err, api.CodeIO); ae.Msg != "attachment exceeds 100 MiB" {
		t.Fatalf("msg %q", ae.Msg)
	}
	if len(*urls) != 0 {
		t.Fatal("the size is known up front, nothing may be fetched")
	}
	if _, statErr := os.Stat(dest); statErr == nil {
		t.Fatal("destination must not exist")
	}
}
