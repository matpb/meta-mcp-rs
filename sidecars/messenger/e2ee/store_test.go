package e2ee

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"testing"

	"github.com/rs/zerolog"
	"go.mau.fi/whatsmeow/proto/waAdv"
	"go.mau.fi/whatsmeow/types"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

func openStore(t *testing.T, dir string) *Store {
	t.Helper()
	s, err := Open(context.Background(), dir, zerolog.Nop())
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	return s
}

func mkMsg(thread string, n int) api.Message {
	text := fmt.Sprintf("text %d", n)
	return api.Message{
		ID:          fmt.Sprintf("wa:%s:s:%06d", thread, n),
		ThreadID:    thread,
		TS:          int64(n) * 1000,
		Sender:      api.Sender{ID: "s", Name: "S"},
		Text:        &text,
		Attachments: []api.Attachment{},
		Reactions:   []api.Reaction{},
		Encrypted:   true,
		Kind:        api.MsgText,
	}
}

func put(t *testing.T, s *Store, m api.Message, media []api.E2EEMedia) {
	t.Helper()
	if err := s.Put(context.Background(), m, m.TS, media); err != nil {
		t.Fatalf("put %s: %v", m.ID, err)
	}
}

func ids(ms []api.Message) []string {
	out := make([]string, len(ms))
	for i, m := range ms {
		out[i] = m.ID
	}
	return out
}

func wantIDs(t *testing.T, got []api.Message, want ...string) {
	t.Helper()
	g := ids(got)
	if fmt.Sprint(g) != fmt.Sprint(want) {
		t.Fatalf("ids = %v, want %v", g, want)
	}
}

func TestOpenCreatesDeviceStore(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	s := openStore(t, dir)
	defer s.Close()
	fi, err := os.Stat(filepath.Join(dir, DBName))
	if err != nil {
		t.Fatal(err)
	}
	if fi.Mode().Perm() != 0o600 {
		t.Fatalf("mode = %v", fi.Mode().Perm())
	}
	for _, suffix := range []string{"-wal", "-shm"} {
		if fi, err := os.Stat(filepath.Join(dir, DBName+suffix)); err == nil && fi.Mode().Perm() != 0o600 {
			t.Fatalf("%s mode = %v", suffix, fi.Mode().Perm())
		}
	}
	dev, err := s.Container().GetFirstDevice(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if dev.ID != nil {
		t.Fatal("fresh device should be unregistered")
	}
	if s.Container().NewDevice() == nil {
		t.Fatal("NewDevice nil")
	}

	jid := types.JID{User: "100000000000001", Device: 7, Server: types.MessengerServer}
	dev.ID = &jid
	dev.Account = &waAdv.ADVSignedDeviceIdentity{
		Details: []byte{1}, AccountSignature: make([]byte, 64), AccountSignatureKey: make([]byte, 32), DeviceSignature: make([]byte, 64),
	}
	if err := dev.Save(ctx); err != nil {
		t.Fatalf("save device: %v", err)
	}
	_ = s.Close()
	s = openStore(t, dir)
	defer s.Close()
	again, err := s.Container().GetFirstDevice(ctx)
	if err != nil || again.ID == nil || again.ID.User != jid.User {
		t.Fatalf("device not persisted: %v %v", again, err)
	}
}

func TestOpenTightensMode(t *testing.T) {
	dir := t.TempDir()
	p := filepath.Join(dir, DBName)
	if err := os.WriteFile(p, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	s := openStore(t, dir)
	defer s.Close()
	fi, _ := os.Stat(p)
	if fi.Mode().Perm() != 0o600 {
		t.Fatalf("mode = %v", fi.Mode().Perm())
	}
}

func TestReopenKeepsMessages(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	s := openStore(t, dir)
	m := mkMsg("t1", 1)
	put(t, s, m, []api.E2EEMedia{{AttachmentID: "a1", MediaType: "WhatsApp Image Keys", Integral: []byte{1, 2}, Mime: "image/png", Size: 9}})
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	s = openStore(t, dir)
	defer s.Close()
	got, ok, err := s.Get(ctx, m.ID)
	if err != nil || !ok || *got.Text != "text 1" {
		t.Fatalf("get: %v %v %v", got, ok, err)
	}
	if _, ok, _ := s.Media(ctx, m.ID, "a1"); !ok {
		t.Fatal("media lost on reopen")
	}
	list, more, err := s.List(ctx, "t1", nil, "", 10)
	if err != nil || more {
		t.Fatalf("list: %v %v", more, err)
	}
	wantIDs(t, list, m.ID)
}

func TestListPaging(t *testing.T) {
	ctx := context.Background()
	s := openStore(t, t.TempDir())
	defer s.Close()
	for _, n := range []int{1, 2, 3} {
		put(t, s, mkMsg("a", n), nil)
	}
	for _, n := range []int{4, 5} {
		put(t, s, mkMsg("b", n), nil)
	}
	a := func(n int) string { return mkMsg("a", n).ID }

	got, more, err := s.List(ctx, "a", nil, "", 2)
	if err != nil || !more {
		t.Fatalf("%v %v", more, err)
	}
	wantIDs(t, got, a(3), a(2))
	got, more, _ = s.List(ctx, "a", nil, "", 3)
	if more {
		t.Fatal("hasMore should be false")
	}
	wantIDs(t, got, a(3), a(2), a(1))

	ts := int64(3000)
	got, more, _ = s.List(ctx, "a", &ts, "", 10)
	wantIDs(t, got, a(2), a(1))
	if more {
		t.Fatal("more")
	}
	got, more, _ = s.List(ctx, "a", nil, a(3), 1)
	wantIDs(t, got, a(2))
	if !more {
		t.Fatal("want more")
	}
	ts = 2500
	got, _, _ = s.List(ctx, "a", &ts, a(2), 10)
	wantIDs(t, got, a(1))

	_, _, err = s.List(ctx, "a", nil, "wa:nope", 5)
	var ae *api.Error
	if !errors.As(err, &ae) || ae.Code != api.CodeMessageNotFound {
		t.Fatalf("err = %v", err)
	}
	_, _, err = s.List(ctx, "b", nil, a(1), 5)
	if !errors.As(err, &ae) || ae.Code != api.CodeMessageNotFound {
		t.Fatalf("cross-thread cursor err = %v", err)
	}
	got, more, _ = s.List(ctx, "empty", nil, "", 5)
	if len(got) != 0 || more || got == nil {
		t.Fatalf("empty thread: %v %v", got, more)
	}
	if _, _, err = s.List(ctx, "a", nil, "", 0); err == nil {
		t.Fatal("limit 0 should fail")
	}
}

func TestSameTimestampOrdersByID(t *testing.T) {
	ctx := context.Background()
	s := openStore(t, t.TempDir())
	defer s.Close()
	for _, id := range []string{"wa:t:s:b", "wa:t:s:a", "wa:t:s:c"} {
		m := mkMsg("t", 1)
		m.ID = id
		put(t, s, m, nil)
	}
	got, more, _ := s.List(ctx, "t", nil, "wa:t:s:c", 5)
	wantIDs(t, got, "wa:t:s:b", "wa:t:s:a")
	if more {
		t.Fatal("more")
	}
}

func TestApplyEditRemovalReaction(t *testing.T) {
	ctx := context.Background()
	s := openStore(t, t.TempDir())
	defer s.Close()
	m1, m2 := mkMsg("t", 1), mkMsg("t", 2)
	att := api.Attachment{ID: "a1", Kind: api.AttImage}
	m1.Attachments = []api.Attachment{att}
	put(t, s, m1, []api.E2EEMedia{{AttachmentID: "a1", MediaType: "x", Integral: []byte{1}, Mime: "image/png", Size: 3}})
	put(t, s, m2, nil)
	// warm the ring so updates are checked against it too
	if _, _, err := s.List(ctx, "t", nil, "", 5); err != nil {
		t.Fatal(err)
	}

	got, ok, err := s.ApplyEdit(ctx, api.Edit{ThreadID: "t", MessageID: m2.ID, Text: "new", SenderID: "s"})
	if err != nil || !ok || *got.Text != "new" || !got.Edited {
		t.Fatalf("edit: %+v %v %v", got, ok, err)
	}
	if _, ok, _ := s.ApplyEdit(ctx, api.Edit{MessageID: "wa:none"}); ok {
		t.Fatal("edit of unknown id")
	}

	love, fire := "love", "fire"
	for i, step := range []struct {
		sender string
		emoji  *string
		want   string
	}{
		{"u1", &love, "u1=love"},
		{"u2", &love, "u1=love u2=love"},
		{"u1", &fire, "u1=fire u2=love"},
		{"u2", nil, "u1=fire"},
		{"u3", nil, "u1=fire"},
	} {
		got, ok, err = s.ApplyReaction(ctx, api.ReactionEvent{ThreadID: "t", MessageID: m2.ID, SenderID: step.sender, Emoji: step.emoji})
		if err != nil || !ok {
			t.Fatalf("step %d: %v %v", i, ok, err)
		}
		var line string
		for _, r := range got.Reactions {
			if line != "" {
				line += " "
			}
			line += r.SenderID + "=" + r.Emoji
		}
		if line != step.want {
			t.Fatalf("step %d: %q, want %q", i, line, step.want)
		}
	}
	if _, ok, _ := s.ApplyReaction(ctx, api.ReactionEvent{MessageID: "wa:none"}); ok {
		t.Fatal("reaction on unknown id")
	}

	list, _, _ := s.List(ctx, "t", nil, "", 5)
	if *list[0].Text != "new" || !list[0].Edited || len(list[0].Reactions) != 1 {
		t.Fatalf("list lost updates: %+v", list[0])
	}
	disk, _, _ := s.Get(ctx, m2.ID)
	if *disk.Text != "new" || len(disk.Reactions) != 1 {
		t.Fatalf("get lost updates: %+v", disk)
	}

	got, ok, err = s.ApplyRemoval(ctx, api.Removal{ThreadID: "t", MessageID: m1.ID, Unsent: true, SenderID: "s"})
	if err != nil || !ok || got.Text != nil || len(got.Attachments) != 0 || got.Attachments == nil || !got.Unsent {
		t.Fatalf("unsend: %+v %v %v", got, ok, err)
	}
	if g, ok, _ := s.Get(ctx, m1.ID); !ok || !g.Unsent || g.Text != nil {
		t.Fatalf("unsent row not kept: %+v %v", g, ok)
	}
	list, _, _ = s.List(ctx, "t", nil, "", 5)
	if len(list) != 2 || !list[1].Unsent {
		t.Fatalf("list after unsend: %v", ids(list))
	}

	if _, ok, _ := s.ApplyRemoval(ctx, api.Removal{MessageID: "wa:none", Unsent: true}); ok {
		t.Fatal("removal of unknown id")
	}
}

func TestMediaRoundTripAndDelete(t *testing.T) {
	ctx := context.Background()
	s := openStore(t, t.TempDir())
	defer s.Close()
	m := mkMsg("t", 1)
	media := []api.E2EEMedia{
		{AttachmentID: "a1", MediaType: "WhatsApp Image Keys", Integral: []byte{1, 2, 3}, Mime: "image/jpeg", Size: 42},
		{AttachmentID: "a2", MediaType: "WhatsApp Video Keys", Integral: []byte{9}, Mime: "video/mp4", Size: 7},
	}
	put(t, s, m, media)
	got, ok, err := s.Media(ctx, m.ID, "a1")
	if err != nil || !ok {
		t.Fatalf("media: %v %v", ok, err)
	}
	want := media[0]
	want.MessageID = m.ID
	if got.MediaType != want.MediaType || string(got.Integral) != string(want.Integral) || got.Mime != want.Mime || got.Size != want.Size || got.MessageID != m.ID || got.AttachmentID != "a1" {
		t.Fatalf("media = %+v", got)
	}
	if _, ok, _ := s.Media(ctx, m.ID, "zz"); ok {
		t.Fatal("unknown attachment found")
	}
	put(t, s, m, nil)
	if _, ok, _ := s.Media(ctx, m.ID, "a2"); !ok {
		t.Fatal("re-put without media dropped media")
	}
	if _, ok, err := s.ApplyRemoval(ctx, api.Removal{ThreadID: "t", MessageID: m.ID}); err != nil || !ok {
		t.Fatalf("delete-for-me: %v %v", ok, err)
	}
	if _, ok, _ := s.Media(ctx, m.ID, "a1"); ok {
		t.Fatal("media survived deletion")
	}
	if _, ok, _ := s.Get(ctx, m.ID); ok {
		t.Fatal("row survived deletion")
	}
	list, more, _ := s.List(ctx, "t", nil, "", 5)
	if len(list) != 0 || more {
		t.Fatalf("list after delete: %v", ids(list))
	}
}

func TestPutMoveAndTimestampChange(t *testing.T) {
	ctx := context.Background()
	s := openStore(t, t.TempDir())
	defer s.Close()
	m := mkMsg("t", 1)
	put(t, s, m, nil)
	put(t, s, mkMsg("t", 2), nil)
	if _, _, err := s.List(ctx, "t", nil, "", 5); err != nil {
		t.Fatal(err)
	}
	if err := s.Put(ctx, m, 9_000_000, nil); err != nil {
		t.Fatal(err)
	}
	list, _, _ := s.List(ctx, "t", nil, "", 5)
	wantIDs(t, list, m.ID, mkMsg("t", 2).ID)
	moved := m
	moved.ThreadID = "u"
	put(t, s, moved, nil)
	list, _, _ = s.List(ctx, "t", nil, "", 5)
	wantIDs(t, list, mkMsg("t", 2).ID)
	list, _, _ = s.List(ctx, "u", nil, "", 5)
	wantIDs(t, list, m.ID)
}

func TestPruneKeepsNewest(t *testing.T) {
	ctx := context.Background()
	defer func(c, sl, r int) { retainCap, pruneSlack, ringCap = c, sl, r }(retainCap, pruneSlack, ringCap)
	s := openStore(t, t.TempDir())
	defer s.Close()
	const total = 2101
	for i := 1; i <= total; i++ {
		m := mkMsg("big", i)
		var media []api.E2EEMedia
		if i == 1 {
			media = []api.E2EEMedia{{AttachmentID: "a", MediaType: "x", Integral: []byte{1}, Mime: "m", Size: 1}}
		}
		put(t, s, m, media)
	}
	put(t, s, mkMsg("other", 1), nil)
	var n int
	if err := s.db.QueryRow(`SELECT COUNT(*) FROM sidecar_e2ee_messages WHERE thread_id = 'big'`).Scan(&n); err != nil {
		t.Fatal(err)
	}
	if n != 2000 {
		t.Fatalf("kept %d, want 2000", n)
	}
	if _, ok, _ := s.Get(ctx, mkMsg("big", 101).ID); ok {
		t.Fatal("row 101 should be pruned")
	}
	if _, ok, _ := s.Get(ctx, mkMsg("big", 102).ID); !ok {
		t.Fatal("row 102 should be kept")
	}
	if _, ok, _ := s.Media(ctx, mkMsg("big", 1).ID, "a"); ok {
		t.Fatal("pruned media kept")
	}
	if _, ok, _ := s.Get(ctx, mkMsg("other", 1).ID); !ok {
		t.Fatal("other thread touched")
	}
	list, more, err := s.List(ctx, "big", nil, "", 5)
	if err != nil || !more {
		t.Fatalf("%v %v", more, err)
	}
	wantIDs(t, list, mkMsg("big", 2101).ID, mkMsg("big", 2100).ID, mkMsg("big", 2099).ID, mkMsg("big", 2098).ID, mkMsg("big", 2097).ID)
}

func TestSmallCapsRingAndDiskAgree(t *testing.T) {
	ctx := context.Background()
	defer func(c, sl, r int) { retainCap, pruneSlack, ringCap = c, sl, r }(retainCap, pruneSlack, ringCap)
	retainCap, pruneSlack, ringCap = 30, 5, 8
	s := openStore(t, t.TempDir())
	defer s.Close()
	for i := 1; i <= 34; i++ {
		put(t, s, mkMsg("t", i), nil)
	}
	var cursor string
	var seen []string
	for {
		page, more, err := s.List(ctx, "t", nil, cursor, 7)
		if err != nil {
			t.Fatal(err)
		}
		seen = append(seen, ids(page)...)
		if !more {
			break
		}
		cursor = page[len(page)-1].ID
	}
	if len(seen) != 34 || seen[0] != mkMsg("t", 34).ID || seen[33] != mkMsg("t", 1).ID {
		t.Fatalf("paged %d rows: %v", len(seen), seen)
	}
	put(t, s, mkMsg("t", 35), nil)
	put(t, s, mkMsg("t", 36), nil)
	var n int
	_ = s.db.QueryRow(`SELECT COUNT(*) FROM sidecar_e2ee_messages`).Scan(&n)
	if n != 30 {
		t.Fatalf("after prune kept %d, want 30", n)
	}
	// an old message inserted below the ring must not appear in the newest page
	old := mkMsg("t", 20)
	old.ID = "wa:t:s:old"
	put(t, s, old, nil)
	page, _, _ := s.List(ctx, "t", nil, "", 3)
	wantIDs(t, page, mkMsg("t", 36).ID, mkMsg("t", 35).ID, mkMsg("t", 34).ID)
	ts := int64(21000)
	page, _, _ = s.List(ctx, "t", &ts, "", 2)
	wantIDs(t, page, "wa:t:s:old", mkMsg("t", 20).ID)
}

func TestWipe(t *testing.T) {
	dir := t.TempDir()
	if err := Wipe(dir); err != nil {
		t.Fatalf("wipe on empty dir: %v", err)
	}
	s := openStore(t, dir)
	put(t, s, mkMsg("t", 1), nil)
	_ = s.Close()
	for _, suffix := range []string{"-wal", "-shm", "-journal"} {
		if err := os.WriteFile(filepath.Join(dir, DBName+suffix), []byte("x"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	if err := Wipe(dir); err != nil {
		t.Fatal(err)
	}
	for _, suffix := range []string{"", "-wal", "-shm", "-journal"} {
		if _, err := os.Stat(filepath.Join(dir, DBName+suffix)); !errors.Is(err, os.ErrNotExist) {
			t.Fatalf("%q still present", suffix)
		}
	}
}

func TestConcurrentPut(t *testing.T) {
	ctx := context.Background()
	s := openStore(t, t.TempDir())
	defer s.Close()
	var wg sync.WaitGroup
	for g := 0; g < 8; g++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for i := 0; i < 50; i++ {
				m := mkMsg("t", g*100+i+1)
				if err := s.Put(ctx, m, m.TS, nil); err != nil {
					t.Errorf("put: %v", err)
					return
				}
				if _, _, err := s.List(ctx, "t", nil, "", 10); err != nil {
					t.Errorf("list: %v", err)
					return
				}
			}
		}()
	}
	wg.Wait()
	list, more, err := s.List(ctx, "t", nil, "", 1000)
	if err != nil || more || len(list) != 400 {
		t.Fatalf("got %d more=%v err=%v", len(list), more, err)
	}
}
