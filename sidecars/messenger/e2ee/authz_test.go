package e2ee

import (
	"context"
	"errors"
	"fmt"
	"testing"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

func TestApplyEditRefusesAnotherSenderOrThread(t *testing.T) {
	ctx := context.Background()
	s := openStore(t, t.TempDir())
	defer s.Close()
	m := mkMsg("t", 1)
	put(t, s, m, nil)
	cases := []struct {
		name string
		e    api.Edit
	}{
		{"other sender", api.Edit{ThreadID: "t", MessageID: m.ID, Text: "forged", SenderID: "u2"}},
		{"no sender", api.Edit{ThreadID: "t", MessageID: m.ID, Text: "forged"}},
		{"other thread", api.Edit{ThreadID: "other", MessageID: m.ID, Text: "forged", SenderID: "s"}},
	}
	for _, c := range cases {
		got, found, err := s.ApplyEdit(ctx, c.e)
		if !errors.Is(err, ErrDenied) || !found || got.ID != "" {
			t.Fatalf("%s: got %+v found=%v err=%v", c.name, got, found, err)
		}
	}
	disk, _, _ := s.Get(ctx, m.ID)
	if *disk.Text != "text 1" || disk.Edited {
		t.Fatalf("a refused edit changed the row: %+v", disk)
	}
	if _, found, err := s.ApplyEdit(ctx, api.Edit{ThreadID: "t", MessageID: m.ID, Text: "mine", SenderID: "s"}); err != nil || !found {
		t.Fatalf("the sender's own edit: found=%v err=%v", found, err)
	}
}

func TestApplyRemovalRefusesAnotherSenderUnlessAdmin(t *testing.T) {
	ctx := context.Background()
	s := openStore(t, t.TempDir())
	defer s.Close()
	m := mkMsg("t", 1)
	put(t, s, m, nil)
	for _, r := range []api.Removal{
		{ThreadID: "t", MessageID: m.ID, Unsent: true, SenderID: "u2"},
		{ThreadID: "t", MessageID: m.ID, Unsent: true},
		{ThreadID: "other", MessageID: m.ID, Unsent: true, SenderID: "s"},
		{ThreadID: "other", MessageID: m.ID, Unsent: true, SenderID: "u2", ByAdmin: true},
	} {
		if _, found, err := s.ApplyRemoval(ctx, r); !errors.Is(err, ErrDenied) || !found {
			t.Fatalf("%+v: found=%v err=%v", r, found, err)
		}
	}
	if disk, _, _ := s.Get(ctx, m.ID); disk.Unsent || disk.Text == nil {
		t.Fatalf("a refused unsend changed the row: %+v", disk)
	}
	got, found, err := s.ApplyRemoval(ctx, api.Removal{ThreadID: "t", MessageID: m.ID, Unsent: true, SenderID: "admin", ByAdmin: true})
	if err != nil || !found || !got.Unsent {
		t.Fatalf("a known admin may unsend: %+v found=%v err=%v", got, found, err)
	}

	own := mkMsg("t", 2)
	put(t, s, own, nil)
	if got, found, err := s.ApplyRemoval(ctx, api.Removal{ThreadID: "t", MessageID: own.ID, Unsent: true, SenderID: "s"}); err != nil || !found || !got.Unsent {
		t.Fatalf("the sender may unsend: %+v found=%v err=%v", got, found, err)
	}
}

func fbRow(n int, ts int64) FBAttachments {
	u := fmt.Sprintf("https://cdn.example/%d", n)
	return FBAttachments{
		MessageID: fmt.Sprintf("fb:mid.$%04d", n), ThreadID: "t", TSMS: ts,
		Attachments: []api.Attachment{{ID: "0", Kind: api.AttImage, URL: &u}},
	}
}

func TestFBAttachmentsRoundTripUpdateAndDrop(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	s := openStore(t, dir)
	row := fbRow(1, 100)
	if err := s.PutFBAttachments(ctx, []FBAttachments{row}); err != nil {
		t.Fatal(err)
	}
	_ = s.Close()

	s = openStore(t, dir)
	defer s.Close()
	got, found, err := s.FBAttachments(ctx, row.MessageID)
	if err != nil || !found || got.ThreadID != "t" || got.TSMS != 100 || len(got.Attachments) != 1 || *got.Attachments[0].URL != "https://cdn.example/1" {
		t.Fatalf("round trip: %+v found=%v err=%v", got, found, err)
	}
	u := "https://cdn.example/fresh"
	row.Attachments[0].URL = &u
	if err := s.PutFBAttachments(ctx, []FBAttachments{row}); err != nil {
		t.Fatal(err)
	}
	if got, _, _ := s.FBAttachments(ctx, row.MessageID); *got.Attachments[0].URL != u {
		t.Fatalf("upsert kept the old link: %+v", got)
	}
	if err := s.DropFBAttachments(ctx, row.MessageID); err != nil {
		t.Fatal(err)
	}
	if _, found, _ := s.FBAttachments(ctx, row.MessageID); found {
		t.Fatal("dropped record still there")
	}
	if err := s.PutFBAttachments(ctx, nil); err != nil {
		t.Fatalf("empty batch: %v", err)
	}
}

func TestFBAttachmentsKeepTheNewestRows(t *testing.T) {
	oldRetain, oldSlack := attachRetain, attachSlack
	attachRetain, attachSlack = 5, 2
	t.Cleanup(func() { attachRetain, attachSlack = oldRetain, oldSlack })
	ctx := context.Background()
	s := openStore(t, t.TempDir())
	defer s.Close()
	for i := 1; i <= 7; i++ {
		if err := s.PutFBAttachments(ctx, []FBAttachments{fbRow(i, int64(i)*10)}); err != nil {
			t.Fatal(err)
		}
	}
	var n int
	if err := s.db.QueryRow(`SELECT COUNT(*) FROM sidecar_fb_attachments`).Scan(&n); err != nil || n != 7 {
		t.Fatalf("within the slack nothing is pruned: n=%d err=%v", n, err)
	}
	if err := s.PutFBAttachments(ctx, []FBAttachments{fbRow(8, 80)}); err != nil {
		t.Fatal(err)
	}
	if err := s.db.QueryRow(`SELECT COUNT(*) FROM sidecar_fb_attachments`).Scan(&n); err != nil || n != 5 {
		t.Fatalf("after pruning n=%d err=%v", n, err)
	}
	for i := 1; i <= 3; i++ {
		if _, found, _ := s.FBAttachments(ctx, fbRow(i, 0).MessageID); found {
			t.Fatalf("old row %d survived", i)
		}
	}
	for i := 4; i <= 8; i++ {
		if _, found, _ := s.FBAttachments(ctx, fbRow(i, 0).MessageID); !found {
			t.Fatalf("new row %d was pruned", i)
		}
	}
}
