package store

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
)

var ctx = context.Background()

func open(t *testing.T) *Store {
	t.Helper()
	s, err := Open(filepath.Join(t.TempDir(), "messages.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { s.Close() })
	return s
}

func sp(s string) *string { return &s }

func must[T any](v T, err error) T {
	if err != nil {
		panic(err)
	}
	return v
}

func ensure(t *testing.T, s *Store, jid, alt, kind string) Chat {
	t.Helper()
	c, _, err := s.EnsureChat(ctx, jid, alt, kind)
	if err != nil {
		t.Fatal(err)
	}
	return c
}

func putRes(t *testing.T, s *Store, m Msg, unread bool) PutResult {
	t.Helper()
	res, err := s.PutMessage(ctx, m, unread)
	if err != nil {
		t.Fatal(err)
	}
	return res
}

// put stores m and reports whether it was inserted as a new row.
func put(t *testing.T, s *Store, m Msg, unread bool) bool {
	t.Helper()
	return putRes(t, s, m, unread) == PutInserted
}

func msg(chat, id string, ts int64) Msg {
	return Msg{Chat: chat, ID: id, TS: ts, Kind: api.MsgText, Text: sp("t-" + id), Sender: "peer", Attachments: nil}
}

func TestFileModeAndReopen(t *testing.T) {
	path := filepath.Join(t.TempDir(), "messages.db")
	s, err := Open(path)
	if err != nil {
		t.Fatal(err)
	}
	if err := s.SetMeta(ctx, "k", "v"); err != nil {
		t.Fatal(err)
	}
	for _, suffix := range []string{"", "-wal", "-shm"} {
		fi, err := os.Stat(path + suffix)
		if errors.Is(err, os.ErrNotExist) && suffix != "" {
			continue
		}
		if err != nil {
			t.Fatal(err)
		}
		if posixModes && fi.Mode().Perm() != 0o600 {
			t.Fatalf("%s mode %o", path+suffix, fi.Mode().Perm())
		}
	}
	s.Close()
	s, err = Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	if v, ok, _ := s.GetMeta(ctx, "k"); !ok || v != "v" {
		t.Fatalf("meta lost on reopen: %q %v", v, ok)
	}
	var ver int
	if err := s.db.QueryRow("PRAGMA user_version").Scan(&ver); err != nil || ver != 2 {
		t.Fatalf("user_version %d %v", ver, err)
	}
}

func TestMeta(t *testing.T) {
	s := open(t)
	if _, ok, err := s.GetMeta(ctx, "x"); ok || err != nil {
		t.Fatalf("missing: %v %v", ok, err)
	}
	s.SetMeta(ctx, "x", "1")
	s.SetMeta(ctx, "x", "2")
	if v, ok, _ := s.GetMeta(ctx, "x"); !ok || v != "2" {
		t.Fatalf("got %q", v)
	}
}

func TestMessageRoundTripEveryField(t *testing.T) {
	s := open(t)
	ensure(t, s, "111@s.whatsapp.net", "222@lid", api.KindDM)
	in := Msg{
		Chat: "111@s.whatsapp.net", ID: "M1",
		ChatWire: "222@lid", SenderWire: "222@lid",
		Sender: "111@s.whatsapp.net", SenderAlt: "222@lid", SenderName: "Ann",
		TS: 1700000000, FromMe: true, Kind: api.MsgMedia,
		Text: sp("caption"), ReplyTo: sp("M0"),
		Attachments: []api.Attachment{{ID: "0", Kind: api.AttImage, Mime: sp("image/jpeg"), Size: new(int64)}},
		Raw:         []byte{0, 1, 2, 255}, Edited: true, Unsent: false,
	}
	if !put(t, s, in, false) {
		t.Fatal("first put must insert")
	}
	got, ok, err := s.GetMessage(ctx, in.Chat, in.ID)
	if err != nil || !ok {
		t.Fatal(err, ok)
	}
	if !reflect.DeepEqual(got, in) {
		t.Fatalf("round trip differs:\n got %+v\nwant %+v", got, in)
	}
	nilFields := Msg{Chat: "c", ID: "M2", TS: 5, Kind: api.MsgUnsupported}
	put(t, s, nilFields, false)
	got, _, _ = s.GetMessage(ctx, "c", "M2")
	if got.Text != nil || got.ReplyTo != nil || got.Raw != nil || got.Attachments != nil || got.FromMe {
		t.Fatalf("nil fields must stay nil: %+v", got)
	}
	if _, ok, _ := s.GetMessage(ctx, "c", "nope"); ok {
		t.Fatal("unknown message found")
	}
}

func TestChatRoundTripAndLookup(t *testing.T) {
	s := open(t)
	c, created, err := s.EnsureChat(ctx, "111@s.whatsapp.net", "", api.KindDM)
	if err != nil || !created || c.JID != "111@s.whatsapp.net" || c.Kind != api.KindDM {
		t.Fatalf("%+v %v %v", c, created, err)
	}
	c, created, _ = s.EnsureChat(ctx, "111@s.whatsapp.net", "222@lid", api.KindDM)
	if created || c.AltJID != "222@lid" {
		t.Fatalf("alt not filled: %+v %v", c, created)
	}
	for _, id := range []string{"111@s.whatsapp.net", "222@lid"} {
		g, ok, err := s.GetChat(ctx, id)
		if err != nil || !ok || g.JID != "111@s.whatsapp.net" || g.AltJID != "222@lid" {
			t.Fatalf("lookup %s: %+v %v %v", id, g, ok, err)
		}
	}
	if _, created, _ := s.EnsureChat(ctx, "222@lid", "111@s.whatsapp.net", api.KindDM); created {
		t.Fatal("ensure by the alt form must find the existing chat")
	}
	if _, ok, _ := s.GetChat(ctx, "zzz"); ok {
		t.Fatal("unknown chat found")
	}
	want := Chat{
		JID: "111@s.whatsapp.net", AltJID: "222@lid", Kind: api.KindDM, Name: "Ann", LastTS: 9, LastPreview: "p",
		UnreadCount: 3, Archived: true, Pinned: true, MutedUntil: 77, ReadUpToTS: 5, EndOfHistory: "COMPLETE_BUT_MORE_MESSAGE_REMAIN_ON_PRIMARY",
		Participants: []api.Participant{{ID: "a", Name: "A", IsMe: true}}, GroupInfoAt: 123,
	}
	got, ok, err := s.UpdateChat(ctx, "222@lid", func(c *Chat) {
		*c = want
		c.JID = "hacked"
	})
	if err != nil || !ok || !reflect.DeepEqual(got, want) {
		t.Fatalf("update: %+v %v %v", got, ok, err)
	}
	g, _, _ := s.GetChat(ctx, want.JID)
	if !reflect.DeepEqual(g, want) {
		t.Fatalf("persisted differs:\n got %+v\nwant %+v", g, want)
	}
	if _, ok, _ := s.UpdateChat(ctx, "nope", func(*Chat) { t.Fatal("fn must not run") }); ok {
		t.Fatal("update of unknown chat reported found")
	}
	if g, _, _ := s.GetChat(ctx, want.JID); g.Participants == nil {
		t.Fatal("participants lost")
	}
	ensure(t, s, "g1@g.us", "", api.KindGroup)
	if g, _, _ := s.GetChat(ctx, "g1@g.us"); g.Participants != nil {
		t.Fatalf("unknown participants must stay nil: %#v", g.Participants)
	}
}

func TestListChatsOrderAndCursorPaging(t *testing.T) {
	s := open(t)
	type spec struct {
		jid string
		ts  int64
	}
	specs := []spec{{"a", 100}, {"b", 100}, {"c", 100}, {"d", 200}, {"e", 50}, {"f", 100}, {"g", 0}}
	for _, sc := range specs {
		ensure(t, s, sc.jid, "", api.KindDM)
		s.UpdateChat(ctx, sc.jid, func(c *Chat) { c.LastTS = sc.ts })
	}
	want := []string{"d", "f", "c", "b", "a", "e", "g"}
	var got []string
	cursor := ""
	pages := 0
	for {
		chats, next, err := s.ListChats(ctx, 3, cursor)
		if err != nil {
			t.Fatal(err)
		}
		pages++
		for _, c := range chats {
			got = append(got, c.JID)
		}
		if next == "" {
			break
		}
		if len(chats) != 3 {
			t.Fatalf("non-final page has %d rows", len(chats))
		}
		cursor = next
	}
	if pages != 3 || !reflect.DeepEqual(got, want) {
		t.Fatalf("pages=%d got %v want %v", pages, got, want)
	}
	all, next, _ := s.ListChats(ctx, 7, "")
	if len(all) != 7 || next != "" {
		t.Fatalf("exact fit must end: %d %q", len(all), next)
	}
	part, next, _ := s.ListChats(ctx, 6, "")
	if len(part) != 6 || next == "" {
		t.Fatalf("one left must give a cursor: %d %q", len(part), next)
	}
}

func TestBadCursor(t *testing.T) {
	s := open(t)
	ensure(t, s, "a", "", api.KindDM)
	for _, cur := range []string{"!!!", "bm90IGpzb24", "e30", "e1widFwiOjF9"} {
		if _, _, err := s.ListChats(ctx, 5, cur); !errors.Is(err, ErrBadCursor) {
			t.Fatalf("cursor %q: %v", cur, err)
		}
	}
}

func TestFetchMessagesPagingAndMore(t *testing.T) {
	s := open(t)
	ensure(t, s, "c", "", api.KindDM)
	for i := 1; i <= 7; i++ {
		ts := int64(100 + i/2*10)
		put(t, s, msg("c", fmt.Sprintf("m%d", i), ts), false)
	}
	put(t, s, msg("other", "x", 500), false)
	var ids []string
	var beforeMS int64
	beforeID := ""
	for page := 0; ; page++ {
		ms, more, err := s.FetchMessages(ctx, "c", 3, beforeMS, beforeID)
		if err != nil {
			t.Fatal(err)
		}
		for _, m := range ms {
			ids = append(ids, m.ID)
		}
		if page == 2 {
			if more || len(ms) != 1 {
				t.Fatalf("last page: more=%v len=%d", more, len(ms))
			}
			break
		}
		if !more || len(ms) != 3 {
			t.Fatalf("page %d: more=%v len=%d", page, more, len(ms))
		}
		last := ms[len(ms)-1]
		beforeMS, beforeID = last.TS*1000, last.ID
	}
	want := []string{"m7", "m6", "m5", "m4", "m3", "m2", "m1"}
	if !reflect.DeepEqual(ids, want) {
		t.Fatalf("got %v want %v", ids, want)
	}
	exact, more, _ := s.FetchMessages(ctx, "c", 7, 0, "")
	if len(exact) != 7 || more {
		t.Fatalf("exact fit: %d %v", len(exact), more)
	}
	strict, _, _ := s.FetchMessages(ctx, "c", 10, 130*1000, "")
	for _, m := range strict {
		if m.TS >= 130 {
			t.Fatalf("empty before_id must be strict on ts: %+v", m)
		}
	}
	if len(strict) != 5 {
		t.Fatalf("strict count %d", len(strict))
	}
	if ms, more, err := s.FetchMessages(ctx, "empty", 5, 0, ""); err != nil || len(ms) != 0 || more {
		t.Fatalf("%v %v %v", ms, more, err)
	}
	old, ok, _ := s.OldestMessage(ctx, "c")
	if !ok || old.ID != "m1" {
		t.Fatalf("oldest %+v %v", old, ok)
	}
	if _, ok, _ := s.OldestMessage(ctx, "empty"); ok {
		t.Fatal("oldest of empty chat")
	}
}

func TestUnreadCountingRules(t *testing.T) {
	s := open(t)
	ensure(t, s, "c", "", api.KindDM)
	count := func() int { c, _, _ := s.GetChat(ctx, "c"); return c.UnreadCount }
	in1 := msg("c", "a", 100)
	put(t, s, in1, true)
	if count() != 1 {
		t.Fatalf("incoming counted: %d", count())
	}
	put(t, s, in1, true)
	if count() != 1 {
		t.Fatalf("redelivery must not recount: %d", count())
	}
	out := msg("c", "b", 101)
	out.FromMe = true
	put(t, s, out, true)
	if count() != 1 {
		t.Fatalf("own message counted: %d", count())
	}
	put(t, s, msg("c", "h", 90), false)
	if count() != 1 {
		t.Fatalf("history message counted: %d", count())
	}
	s.UpdateChat(ctx, "c", func(c *Chat) { c.ReadUpToTS = 150; c.UnreadCount = 0 })
	put(t, s, msg("c", "old", 140), true)
	if count() != 0 {
		t.Fatalf("message at or below ReadUpToTS counted: %d", count())
	}
	put(t, s, msg("c", "new", 151), true)
	if count() != 1 {
		t.Fatalf("newer message not counted: %d", count())
	}
}

func TestLastTSAndPreview(t *testing.T) {
	s := open(t)
	ensure(t, s, "c", "", api.KindDM)
	put(t, s, msg("c", "a", 100), false)
	put(t, s, msg("c", "old", 50), false)
	c, _, _ := s.GetChat(ctx, "c")
	if c.LastTS != 100 || c.LastPreview != "t-a" {
		t.Fatalf("older message moved last: %+v", c)
	}
	tie := msg("c", "b", 100)
	tie.Text = sp(strings.Repeat("é", 150))
	put(t, s, tie, false)
	c, _, _ = s.GetChat(ctx, "c")
	if c.LastTS != 100 || c.LastPreview != strings.Repeat("é", 100) {
		t.Fatalf("tie must take the later put, 100 runes: %d %q", c.LastTS, c.LastPreview)
	}
	img := Msg{Chat: "c", ID: "i", TS: 200, Kind: api.MsgMedia, Attachments: []api.Attachment{{ID: "0", Kind: api.AttImage}}}
	put(t, s, img, false)
	if c, _, _ = s.GetChat(ctx, "c"); c.LastPreview != "[image]" || c.LastTS != 200 {
		t.Fatalf("%+v", c)
	}
	ptt := Msg{Chat: "c", ID: "p", TS: 201, Kind: api.MsgMedia, Attachments: []api.Attachment{{ID: "0", Kind: api.AttAudio, Mime: sp("audio/ogg; codecs=opus")}}}
	put(t, s, ptt, false)
	if c, _, _ = s.GetChat(ctx, "c"); c.LastPreview != "[voice note]" {
		t.Fatalf("%q", c.LastPreview)
	}
	s.UpdateMessage(ctx, "c", "p", func(m *Msg) { m.Text = sp("edited"); m.Edited = true })
	if c, _, _ = s.GetChat(ctx, "c"); c.LastPreview != "edited" {
		t.Fatalf("edit of newest must refresh preview: %q", c.LastPreview)
	}
	s.UpdateMessage(ctx, "c", "a", func(m *Msg) { m.Text = sp("zzz") })
	if c, _, _ = s.GetChat(ctx, "c"); c.LastPreview != "edited" {
		t.Fatalf("edit of older must not touch preview: %q", c.LastPreview)
	}
}

func TestPutMessageIdempotentAndKeepsUnsent(t *testing.T) {
	s := open(t)
	ensure(t, s, "c", "", api.KindDM)
	m := msg("c", "a", 100)
	if !put(t, s, m, true) || put(t, s, m, true) {
		t.Fatal("insert then redelivery must report inserted true then false")
	}
	if ms, _, _ := s.FetchMessages(ctx, "c", 10, 0, ""); len(ms) != 1 {
		t.Fatalf("duplicate row: %d", len(ms))
	}
	rev, ok, err := s.UpdateMessage(ctx, "c", "a", func(m *Msg) { m.Unsent = true; m.Text = nil })
	if err != nil || !ok || !rev.Unsent {
		t.Fatal(rev, ok, err)
	}
	put(t, s, m, true)
	got, _, _ := s.GetMessage(ctx, "c", "a")
	if !got.Unsent || got.Text != nil {
		t.Fatalf("redelivery resurrected a revoked message: %+v", got)
	}
	changed := m
	changed.Text = sp("changed")
	changed.Edited = true
	put(t, s, changed, true)
	got, _, _ = s.GetMessage(ctx, "c", "a")
	if !got.Unsent || got.Text == nil || *got.Text != "changed" {
		t.Fatalf("explicit update must apply and keep unsent: %+v", got)
	}
	m2 := msg("c", "e", 200)
	put(t, s, m2, false)
	s.UpdateMessage(ctx, "c", "e", func(m *Msg) { m.Edited = true; m.Text = sp("v2") })
	put(t, s, m2, false)
	if got, _, _ = s.GetMessage(ctx, "c", "e"); got.Text == nil || *got.Text != "v2" || !got.Edited {
		t.Fatalf("stale redelivery reverted an edit: %+v", got)
	}
	if _, ok, _ := s.UpdateMessage(ctx, "c", "nope", func(*Msg) { t.Fatal("fn must not run") }); ok {
		t.Fatal("unknown message updated")
	}
}

func TestReactionsReflectedInFetch(t *testing.T) {
	s := open(t)
	ensure(t, s, "c", "", api.KindDM)
	put(t, s, msg("c", "a", 10), false)
	put(t, s, msg("c", "b", 11), false)
	s.PutReaction(ctx, "c", "a", "u1", "👍", 1)
	s.PutReaction(ctx, "c", "a", "u2", "🎉", 2)
	s.PutReaction(ctx, "c", "b", "u1", "❤️", 3)
	ms, _, _ := s.FetchMessages(ctx, "c", 10, 0, "")
	byID := map[string]Msg{}
	for _, m := range ms {
		byID[m.ID] = m
	}
	wantA := []api.Reaction{{SenderID: "u1", Emoji: "👍"}, {SenderID: "u2", Emoji: "🎉"}}
	if !reflect.DeepEqual(byID["a"].Reactions, wantA) || len(byID["b"].Reactions) != 1 {
		t.Fatalf("reactions: %+v", byID)
	}
	s.PutReaction(ctx, "c", "a", "u1", "😂", 4)
	s.PutReaction(ctx, "c", "a", "u2", "", 5)
	g, _, _ := s.GetMessage(ctx, "c", "a")
	if !reflect.DeepEqual(g.Reactions, []api.Reaction{{SenderID: "u1", Emoji: "😂"}}) {
		t.Fatalf("replace and remove: %+v", g.Reactions)
	}
	s.PutReaction(ctx, "c", "a", "u1", "", 6)
	g, _, _ = s.GetMessage(ctx, "c", "a")
	if len(g.Reactions) != 0 {
		t.Fatalf("not removed: %+v", g.Reactions)
	}
	if got := g.API(); got.Reactions == nil || got.Attachments == nil {
		t.Fatal("API slices must not be nil")
	}
	u, _, _ := s.UpdateMessage(ctx, "c", "b", func(m *Msg) {})
	if len(u.Reactions) != 1 {
		t.Fatalf("UpdateMessage result lacks reactions: %+v", u)
	}
}

func TestReceiptsKeepMax(t *testing.T) {
	s := open(t)
	s.PutReceipt(ctx, "c", "u", "read", 10)
	s.PutReceipt(ctx, "c", "u", "read", 5)
	s.PutReceipt(ctx, "c", "u", "delivered", 3)
	s.PutReceipt(ctx, "c", "u", "read", 12)
	rows, err := s.db.Query("SELECT kind, ts FROM receipts WHERE chat='c' AND sender='u' ORDER BY kind")
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()
	got := map[string]int64{}
	for rows.Next() {
		var k string
		var ts int64
		rows.Scan(&k, &ts)
		got[k] = ts
	}
	if !reflect.DeepEqual(got, map[string]int64{"read": 12, "delivered": 3}) {
		t.Fatalf("%v", got)
	}
}

func count(t *testing.T, s *Store, table, chatCol, chat string) int {
	t.Helper()
	var n int
	if err := s.db.QueryRow("SELECT COUNT(*) FROM "+table+" WHERE "+chatCol+" = ?", chat).Scan(&n); err != nil {
		t.Fatal(err)
	}
	return n
}

func TestMergeChatMovesRows(t *testing.T) {
	s := open(t)
	ensure(t, s, "lid1@lid", "", api.KindDM)
	ensure(t, s, "pn1@s.whatsapp.net", "", api.KindDM)
	s.UpdateChat(ctx, "lid1@lid", func(c *Chat) { c.Name = ""; c.UnreadCount = 2; c.ReadUpToTS = 40 })
	s.UpdateChat(ctx, "pn1@s.whatsapp.net", func(c *Chat) { c.Name = "Ann"; c.Pinned = true })
	put(t, s, msg("lid1@lid", "a", 300), true)
	put(t, s, msg("lid1@lid", "dup", 100), false)
	put(t, s, msg("pn1@s.whatsapp.net", "b", 200), false)
	put(t, s, msg("pn1@s.whatsapp.net", "dup", 100), false)
	s.PutReaction(ctx, "lid1@lid", "a", "u", "👍", 1)
	s.PutReceipt(ctx, "lid1@lid", "u", "read", 9)
	s.PutReceipt(ctx, "pn1@s.whatsapp.net", "u", "read", 4)
	if err := s.MergeChat(ctx, "lid1@lid", "pn1@s.whatsapp.net"); err != nil {
		t.Fatal(err)
	}
	if n := count(t, s, "chats", "jid", "lid1@lid"); n != 0 {
		t.Fatal("source chat row survived")
	}
	for _, tbl := range []struct{ t, col string }{{"messages", "chat"}, {"reactions", "chat"}, {"receipts", "chat"}} {
		if n := count(t, s, tbl.t, tbl.col, "lid1@lid"); n != 0 {
			t.Fatalf("%s still has %d rows on the source", tbl.t, n)
		}
	}
	ms, _, _ := s.FetchMessages(ctx, "pn1@s.whatsapp.net", 10, 0, "")
	if len(ms) != 3 || ms[0].ID != "a" {
		t.Fatalf("messages: %+v", ms)
	}
	if len(ms[0].Reactions) != 1 {
		t.Fatalf("reaction not moved: %+v", ms[0])
	}
	var ts int64
	s.db.QueryRow("SELECT ts FROM receipts WHERE chat='pn1@s.whatsapp.net' AND sender='u' AND kind='read'").Scan(&ts)
	if ts != 9 {
		t.Fatalf("receipt max: %d", ts)
	}
	c, ok, _ := s.GetChat(ctx, "pn1@s.whatsapp.net")
	if !ok || c.Name != "Ann" || c.LastTS != 300 || c.UnreadCount != 3 || !c.Pinned || c.ReadUpToTS != 40 || c.AltJID != "lid1@lid" {
		t.Fatalf("merged meta: %+v", c)
	}
	if g, ok, _ := s.GetChat(ctx, "lid1@lid"); !ok || g.JID != "pn1@s.whatsapp.net" {
		t.Fatalf("old id must resolve through alt: %+v %v", g, ok)
	}
}

func TestMergeChatIntoMissingTarget(t *testing.T) {
	s := open(t)
	ensure(t, s, "lid1@lid", "", api.KindDM)
	put(t, s, msg("lid1@lid", "a", 5), false)
	if err := s.MergeChat(ctx, "lid1@lid", "pn@s.whatsapp.net"); err != nil {
		t.Fatal(err)
	}
	c, ok, _ := s.GetChat(ctx, "pn@s.whatsapp.net")
	if !ok || c.JID != "pn@s.whatsapp.net" || c.AltJID != "lid1@lid" || c.LastTS != 5 {
		t.Fatalf("%+v %v", c, ok)
	}
	if _, ok, _ := s.GetMessage(ctx, "pn@s.whatsapp.net", "a"); !ok {
		t.Fatal("message not moved")
	}
	if err := s.MergeChat(ctx, "nothing", "pn@s.whatsapp.net"); err != nil {
		t.Fatal(err)
	}
}

func TestDeleteChat(t *testing.T) {
	s := open(t)
	ensure(t, s, "a", "alt-a", api.KindDM)
	ensure(t, s, "b", "", api.KindDM)
	put(t, s, msg("a", "1", 1), false)
	put(t, s, msg("b", "1", 1), false)
	s.PutReaction(ctx, "a", "1", "u", "👍", 1)
	s.PutReceipt(ctx, "a", "u", "read", 1)
	if err := s.DeleteChat(ctx, "alt-a"); err != nil {
		t.Fatal(err)
	}
	if _, ok, _ := s.GetChat(ctx, "a"); ok {
		t.Fatal("chat survived")
	}
	for _, tbl := range []string{"messages", "reactions", "receipts"} {
		if n := count(t, s, tbl, "chat", "a"); n != 0 {
			t.Fatalf("%s kept %d rows", tbl, n)
		}
	}
	if n := count(t, s, "messages", "chat", "b"); n != 1 {
		t.Fatal("other chat touched")
	}
	if err := s.DeleteChat(ctx, "missing"); err != nil {
		t.Fatal(err)
	}
}

func TestReset(t *testing.T) {
	s := open(t)
	ensure(t, s, "a", "", api.KindDM)
	put(t, s, msg("a", "1", 1), false)
	s.PutReaction(ctx, "a", "1", "u", "👍", 1)
	s.PutReceipt(ctx, "a", "u", "read", 1)
	s.PutContactName(ctx, "u", "Full", "First")
	s.PutLIDMapping(ctx, "l@lid", "p@s.whatsapp.net")
	s.SetMeta(ctx, "owner_jid", "x")
	if err := s.Reset(ctx); err != nil {
		t.Fatal(err)
	}
	for _, tbl := range []string{"meta", "chats", "messages", "reactions", "receipts", "contacts", "lid_map"} {
		var n int
		s.db.QueryRow("SELECT COUNT(*) FROM " + tbl).Scan(&n)
		if n != 0 {
			t.Fatalf("%s has %d rows", tbl, n)
		}
	}
	ensure(t, s, "a", "", api.KindDM)
}

func TestLIDCache(t *testing.T) {
	s := open(t)
	if _, ok, err := s.PNForLID(ctx, "l1@lid"); ok || err != nil {
		t.Fatal(ok, err)
	}
	s.PutLIDMapping(ctx, "l1@lid", "p1@s.whatsapp.net")
	if pn, ok, _ := s.PNForLID(ctx, "l1@lid"); !ok || pn != "p1@s.whatsapp.net" {
		t.Fatal(pn, ok)
	}
	if l, ok, _ := s.LIDForPN(ctx, "p1@s.whatsapp.net"); !ok || l != "l1@lid" {
		t.Fatal(l, ok)
	}
	s.PutLIDMapping(ctx, "l1@lid", "p2@s.whatsapp.net")
	if _, ok, _ := s.LIDForPN(ctx, "p1@s.whatsapp.net"); ok {
		t.Fatal("old pn still mapped")
	}
	if pn, _, _ := s.PNForLID(ctx, "l1@lid"); pn != "p2@s.whatsapp.net" {
		t.Fatal(pn)
	}
	if err := s.PutLIDMapping(ctx, "l2@lid", "p2@s.whatsapp.net"); err != nil {
		t.Fatalf("pn re-pointed to another lid: %v", err)
	}
	if l, _, _ := s.LIDForPN(ctx, "p2@s.whatsapp.net"); l != "l2@lid" {
		t.Fatal(l)
	}
	if _, ok, _ := s.PNForLID(ctx, "l1@lid"); ok {
		t.Fatal("stale lid kept")
	}
}

func TestContactNamePrecedence(t *testing.T) {
	s := open(t)
	if _, ok, err := s.ContactName(ctx, "u"); ok || err != nil {
		t.Fatal(ok, err)
	}
	s.PutPushName(ctx, "u", "Push")
	if n, ok, _ := s.ContactName(ctx, "u"); !ok || n != "Push" {
		t.Fatal(n, ok)
	}
	s.PutContactName(ctx, "u", "", "First")
	if n, _, _ := s.ContactName(ctx, "u"); n != "First" {
		t.Fatal(n)
	}
	s.PutContactName(ctx, "u", "Full Name", "First")
	if n, _, _ := s.ContactName(ctx, "u"); n != "Full Name" {
		t.Fatal(n)
	}
	s.PutPushName(ctx, "u", "Other")
	s.PutPushName(ctx, "u", "")
	if n, _, _ := s.ContactName(ctx, "u"); n != "Full Name" {
		t.Fatal(n)
	}
	s.PutContactName(ctx, "p", "", "")
	if _, ok, _ := s.ContactName(ctx, "p"); ok {
		t.Fatal("empty names must report not found")
	}
	var push string
	s.db.QueryRow("SELECT push_name FROM contacts WHERE jid='u'").Scan(&push)
	if push != "Other" {
		t.Fatalf("PutContactName must keep push_name, empty PutPushName ignored: %q", push)
	}
}

func TestUnreadIncoming(t *testing.T) {
	s := open(t)
	ensure(t, s, "c", "", api.KindDM)
	for i := int64(1); i <= 6; i++ {
		m := msg("c", fmt.Sprintf("m%d", i), i*10)
		m.FromMe = i == 4
		put(t, s, m, false)
	}
	ms, err := s.UnreadIncoming(ctx, "c", Mark{TS: 10, ID: "m1"}, 50, nil, 100)
	if err != nil {
		t.Fatal(err)
	}
	var ids []string
	for _, m := range ms {
		ids = append(ids, m.ID)
	}
	if !reflect.DeepEqual(ids, []string{"m5", "m3", "m2"}) {
		t.Fatalf("%v", ids)
	}
	ms, _ = s.UnreadIncoming(ctx, "c", Mark{}, 1<<62, nil, 2)
	if len(ms) != 2 || ms[0].ID != "m6" {
		t.Fatalf("max/newest first: %+v", ms)
	}
	if ms[0].ChatWire != "" || ms[0].Text != nil || ms[0].Chat != "c" || ms[0].TS != 60 {
		t.Fatalf("only the receipt fields are filled: %+v", ms[0])
	}
}

func TestAPIConversions(t *testing.T) {
	th := Chat{JID: "j", Kind: api.KindDM}.API()
	if th.ID != "j" || th.AltID != nil || th.Name != nil || th.LastMessageTS != nil || th.LastMessagePreview != nil ||
		th.Participants == nil || len(th.Participants) != 0 || th.Unread == nil || *th.Unread || !th.Encrypted {
		t.Fatalf("empty chat: %+v", th)
	}
	th = Chat{JID: "j", AltJID: "a", Kind: api.KindGroup, Name: "N", LastTS: 7, LastPreview: "p", UnreadCount: 2,
		Participants: []api.Participant{{ID: "x"}}}.API()
	if *th.AltID != "a" || *th.Name != "N" || *th.LastMessageTS != 7 || *th.LastMessagePreview != "p" || !*th.Unread || len(th.Participants) != 1 {
		t.Fatalf("full chat: %+v", th)
	}
	m := Msg{Chat: "c", ID: "i", TS: 3, Sender: "s", SenderName: "S", FromMe: true, Kind: api.MsgText, Text: sp("hi"), ReplyTo: sp("r"), Edited: true}.API()
	if m.ThreadID != "c" || m.Sender != (api.Sender{ID: "s", Name: "S", IsMe: true}) || m.Attachments == nil || m.Reactions == nil ||
		!m.Encrypted || !m.Edited || *m.Text != "hi" || *m.ReplyTo != "r" {
		t.Fatalf("%+v", m)
	}
}

// v1DDL is the schema of messages.db before sender keying, copied from the first release.
const v1DDL = `
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE chats (
	jid TEXT PRIMARY KEY, alt_jid TEXT NOT NULL DEFAULT '', kind TEXT NOT NULL, name TEXT NOT NULL DEFAULT '',
	last_ts INTEGER NOT NULL DEFAULT 0, last_preview TEXT NOT NULL DEFAULT '', unread_count INTEGER NOT NULL DEFAULT 0,
	archived INTEGER NOT NULL DEFAULT 0, pinned INTEGER NOT NULL DEFAULT 0, muted_until INTEGER NOT NULL DEFAULT 0,
	read_up_to_ts INTEGER NOT NULL DEFAULT 0, end_of_history TEXT NOT NULL DEFAULT '', participants TEXT,
	group_info_at INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX chats_alt_jid ON chats(alt_jid);
CREATE INDEX chats_last ON chats(last_ts DESC, jid DESC);
CREATE TABLE messages (
	chat TEXT NOT NULL, id TEXT NOT NULL, chat_wire TEXT NOT NULL DEFAULT '', sender_wire TEXT NOT NULL DEFAULT '',
	sender TEXT NOT NULL DEFAULT '', sender_alt TEXT NOT NULL DEFAULT '', sender_name TEXT NOT NULL DEFAULT '',
	ts INTEGER NOT NULL, from_me INTEGER NOT NULL DEFAULT 0, kind TEXT NOT NULL, text TEXT, reply_to TEXT,
	attachments TEXT NOT NULL DEFAULT '[]', raw BLOB, edited INTEGER NOT NULL DEFAULT 0, unsent INTEGER NOT NULL DEFAULT 0,
	PRIMARY KEY (chat, id)
);
CREATE INDEX messages_chat_ts ON messages(chat, ts DESC, id DESC);
CREATE TABLE reactions (chat TEXT NOT NULL, message_id TEXT NOT NULL, sender TEXT NOT NULL, emoji TEXT NOT NULL, ts INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (chat, message_id, sender));
CREATE TABLE receipts (chat TEXT NOT NULL, sender TEXT NOT NULL, kind TEXT NOT NULL, ts INTEGER NOT NULL, PRIMARY KEY (chat, sender, kind));
CREATE TABLE contacts (jid TEXT PRIMARY KEY, full_name TEXT NOT NULL DEFAULT '', first_name TEXT NOT NULL DEFAULT '', push_name TEXT NOT NULL DEFAULT '');
CREATE TABLE lid_map (lid TEXT PRIMARY KEY, pn TEXT NOT NULL UNIQUE);
PRAGMA user_version = 1;
`

func TestMigrationFromV1KeepsEveryRowReadable(t *testing.T) {
	path := filepath.Join(t.TempDir(), "messages.db")
	old, err := sql.Open("sqlite", "file:"+path)
	if err != nil {
		t.Fatal(err)
	}
	for _, q := range []string{
		v1DDL,
		`INSERT INTO meta VALUES ('owner_jid', 'me@s.whatsapp.net')`,
		`INSERT INTO chats (jid, alt_jid, kind, name, last_ts, last_preview, unread_count, read_up_to_ts, participants) VALUES ('a@s.whatsapp.net', 'a@lid', 'dm', 'Ann', 300, 'three', 1, 200, '[{"id":"a@s.whatsapp.net","name":"Ann","is_me":false}]')`,
		`INSERT INTO chats (jid, kind, name, read_up_to_ts) VALUES ('g@g.us', 'group', 'Crew', 0)`,
		`INSERT INTO messages (chat, id, chat_wire, sender_wire, sender, sender_alt, sender_name, ts, from_me, kind, text, attachments, raw, edited) VALUES ('a@s.whatsapp.net', 'one', 'a@lid', 'a@lid', 'a@s.whatsapp.net', 'a@lid', 'Ann', 100, 0, 'text', 'hello', '[]', x'0aff', 1)`,
		`INSERT INTO messages (chat, id, sender, ts, from_me, kind, text) VALUES ('a@s.whatsapp.net', 'two', 'me@s.whatsapp.net', 200, 1, 'text', 'reply')`,
		`INSERT INTO messages (chat, id, sender, ts, kind, text) VALUES ('a@s.whatsapp.net', 'zed', 'a@s.whatsapp.net', 200, 'text', 'same second as the mark')`,
		`INSERT INTO messages (chat, id, sender, ts, kind, text) VALUES ('a@s.whatsapp.net', 'three', 'a@s.whatsapp.net', 300, 'text', 'three')`,
		`INSERT INTO reactions VALUES ('a@s.whatsapp.net', 'one', 'me@s.whatsapp.net', '👍', 150)`,
		`INSERT INTO receipts VALUES ('a@s.whatsapp.net', 'a@s.whatsapp.net', 'read', 120)`,
		`INSERT INTO contacts (jid, full_name) VALUES ('a@s.whatsapp.net', 'Ann Liddell')`,
		`INSERT INTO lid_map VALUES ('a@lid', 'a@s.whatsapp.net')`,
	} {
		if _, err := old.Exec(q); err != nil {
			t.Fatalf("seed v1: %v\n%s", err, q)
		}
	}
	if err := old.Close(); err != nil {
		t.Fatal(err)
	}

	s, err := Open(path)
	if err != nil {
		t.Fatalf("open v1 database: %v", err)
	}
	defer s.Close()
	var ver int
	if err := s.db.QueryRow("PRAGMA user_version").Scan(&ver); err != nil || ver != 2 {
		t.Fatalf("user_version %d %v", ver, err)
	}
	ms, more, err := s.FetchMessages(ctx, "a@s.whatsapp.net", 10, 0, "")
	if err != nil || more || len(ms) != 4 || ms[0].ID != "three" || ms[1].ID != "zed" || ms[3].ID != "one" {
		t.Fatalf("migrated messages %v more=%v err=%v", ids(ms), more, err)
	}
	one := ms[3]
	if one.Text == nil || *one.Text != "hello" || one.Sender != "a@s.whatsapp.net" || one.SenderAlt != "a@lid" || one.SenderWire != "a@lid" || !one.Edited ||
		!reflect.DeepEqual(one.Raw, []byte{0x0a, 0xff}) || len(one.Reactions) != 1 || one.Reactions[0].Emoji != "👍" {
		t.Fatalf("migrated row changed: %+v", one)
	}
	c, ok, err := s.GetChat(ctx, "a@lid")
	if err != nil || !ok || c.JID != "a@s.whatsapp.net" || c.Name != "Ann" || c.UnreadCount != 1 || len(c.Participants) != 1 || c.Admins != nil {
		t.Fatalf("migrated chat %+v %v %v", c, ok, err)
	}
	if c.ReadUpToTS != 200 || c.ReadUpToID != "zed" {
		t.Fatalf("watermark id must be the newest id at the mark second, got %d %q", c.ReadUpToTS, c.ReadUpToID)
	}
	if g, _, _ := s.GetChat(ctx, "g@g.us"); g.ReadUpToID != "" {
		t.Fatalf("a chat never marked read keeps an empty watermark id: %+v", g)
	}
	if v, ok, _ := s.GetMeta(ctx, "owner_jid"); !ok || v != "me@s.whatsapp.net" {
		t.Fatalf("meta %q %v", v, ok)
	}
	if name, ok, _ := s.ContactName(ctx, "a@s.whatsapp.net"); !ok || name != "Ann Liddell" {
		t.Fatalf("contact %q %v", name, ok)
	}
	if lid, ok, _ := s.LIDForPN(ctx, "a@s.whatsapp.net"); !ok || lid != "a@lid" {
		t.Fatalf("lid cache %q %v", lid, ok)
	}
	if got := putRes(t, s, Msg{Chat: "a@s.whatsapp.net", ID: "one", TS: 5, Kind: api.MsgText, Sender: "mallory"}, true); got != PutConflict {
		t.Fatalf("a migrated row must be protected like a new one: %v", got)
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	s2, err := Open(path)
	if err != nil {
		t.Fatalf("second open after migration: %v", err)
	}
	s2.Close()
}

func ids(ms []Msg) []string {
	out := make([]string, len(ms))
	for i, m := range ms {
		out[i] = m.ID
	}
	return out
}

func TestMessagesAreKeyedBySenderAndChatIDIsUnique(t *testing.T) {
	s := open(t)
	var pk []string
	rows, err := s.db.Query("SELECT name FROM pragma_table_info('messages') WHERE pk > 0 ORDER BY pk")
	if err != nil {
		t.Fatal(err)
	}
	for rows.Next() {
		var n string
		rows.Scan(&n)
		pk = append(pk, n)
	}
	rows.Close()
	if !reflect.DeepEqual(pk, []string{"chat", "sender", "id"}) {
		t.Fatalf("primary key %v", pk)
	}
	var unique int
	s.db.QueryRow("SELECT COUNT(*) FROM pragma_index_list('messages') WHERE \"unique\" = 1 AND origin = 'c'").Scan(&unique)
	if unique != 1 {
		t.Fatalf("want the unique (chat, id) index, found %d", unique)
	}
}

func TestPutMessageFirstSenderOfAnIDKeepsIt(t *testing.T) {
	s := open(t)
	ensure(t, s, "g@g.us", "", api.KindGroup)
	alice := Msg{Chat: "g@g.us", ID: "X", TS: 100, Kind: api.MsgText, Text: sp("Alice: 5pm"), Sender: "alice@s.whatsapp.net", SenderAlt: "1@lid", SenderWire: "1@lid"}
	if putRes(t, s, alice, true) != PutInserted {
		t.Fatal("first put must insert")
	}
	mallory := alice
	mallory.Sender, mallory.SenderAlt, mallory.SenderWire, mallory.Text, mallory.TS = "mallory@s.whatsapp.net", "2@lid", "2@lid", sp("Mallory: wire money"), 200
	if got := putRes(t, s, mallory, true); got != PutConflict {
		t.Fatalf("another sender reusing the id: %v", got)
	}
	mine := alice
	mine.FromMe, mine.Sender, mine.SenderAlt, mine.SenderWire = true, "me@s.whatsapp.net", "9@lid", "me@s.whatsapp.net"
	mine.Text = sp("forged as me")
	if got := putRes(t, s, mine, true); got != PutConflict {
		t.Fatalf("the owner flag is part of the sender: %v", got)
	}
	got, _, _ := s.GetMessage(ctx, "g@g.us", "X")
	if got.Text == nil || *got.Text != "Alice: 5pm" || got.Sender != alice.Sender || got.TS != 100 {
		t.Fatalf("stored message changed: %+v", got)
	}
	if c, _, _ := s.GetChat(ctx, "g@g.us"); c.UnreadCount != 1 || c.LastTS != 100 || c.LastPreview != "Alice: 5pm" {
		t.Fatalf("a rejected message must not touch the chat: %+v", c)
	}
	// the same message in the other id form of the same sender is a redelivery, not a conflict
	again := alice
	again.Sender, again.SenderAlt, again.SenderWire = "1@lid", "", "1@lid"
	again.Text = sp("rewritten by the same sender")
	if got := putRes(t, s, again, true); got != PutExisting {
		t.Fatalf("same sender in another form: %v", got)
	}
	if stored, _, _ := s.GetMessage(ctx, "g@g.us", "X"); *stored.Text != "Alice: 5pm" {
		t.Fatalf("a same-sender redelivery with other content must be ignored: %q", *stored.Text)
	}
	// only an edit event may change it
	edit := again
	edit.Edited = true
	edit.TS = 999
	if got := putRes(t, s, edit, true); got != PutExisting {
		t.Fatalf("edit: %v", got)
	}
	stored, _, _ := s.GetMessage(ctx, "g@g.us", "X")
	if *stored.Text != "rewritten by the same sender" || !stored.Edited || stored.TS != 100 || stored.Sender != alice.Sender {
		t.Fatalf("edit must change the content only: %+v", stored)
	}
}

func TestUpdateMessageIfDeniedLeavesTheRowAlone(t *testing.T) {
	s := open(t)
	ensure(t, s, "c", "", api.KindDM)
	put(t, s, msg("c", "a", 100), false)
	ran := false
	_, res, err := s.UpdateMessageIf(ctx, "c", "a", func(m Msg) bool { return m.Sender == "someone else" }, func(*Msg) { ran = true })
	if err != nil || res != UpdateDenied || ran {
		t.Fatalf("denied update: %v %v ran=%v", res, err, ran)
	}
	if _, res, _ = s.UpdateMessageIf(ctx, "c", "nope", func(Msg) bool { return true }, func(*Msg) { ran = true }); res != UpdateMissing || ran {
		t.Fatalf("missing: %v ran=%v", res, ran)
	}
	got, res, err := s.UpdateMessageIf(ctx, "c", "a", func(m Msg) bool { return m.Sender == "peer" }, func(m *Msg) { m.Text = sp("new") })
	if err != nil || res != UpdateDone || *got.Text != "new" {
		t.Fatalf("allowed update: %v %v %+v", res, err, got)
	}
}

func TestMessageWrittenBy(t *testing.T) {
	m := Msg{Sender: "pn@s.whatsapp.net", SenderAlt: "1@lid", SenderWire: "1:4@lid"}
	for _, c := range []struct {
		name   string
		fromMe bool
		ids    []string
		want   bool
	}{
		{"canonical", false, []string{"pn@s.whatsapp.net"}, true},
		{"alt", false, []string{"x", "1@lid"}, true},
		{"wire", false, []string{"1:4@lid"}, true},
		{"other", false, []string{"2@lid"}, false},
		{"empty ids never match an empty field", false, []string{"", ""}, false},
		{"owner flag differs", true, []string{"pn@s.whatsapp.net"}, false},
	} {
		if got := m.WrittenBy(c.fromMe, c.ids...); got != c.want {
			t.Errorf("%s: %v want %v", c.name, got, c.want)
		}
	}
	own := Msg{FromMe: true, Sender: "me@s.whatsapp.net"}
	if !own.WrittenBy(true) || own.WrittenBy(false, "me@s.whatsapp.net") {
		t.Error("an owner message belongs to the owner only")
	}
	if (Msg{}).WrittenBy(false, "") {
		t.Error("two empty identities are not the same sender")
	}
}

func TestUnreadWatermarkComparesTimestampAndID(t *testing.T) {
	s := open(t)
	ensure(t, s, "c", "", api.KindDM)
	count := func() int { c, _, _ := s.GetChat(ctx, "c"); return c.UnreadCount }
	s.UpdateChat(ctx, "c", func(c *Chat) { c.ReadUpToTS, c.ReadUpToID = 150, "m"; c.UnreadCount = 0 })
	put(t, s, msg("c", "a", 150), true)
	if count() != 0 {
		t.Fatalf("same second, smaller id counts as read: %d", count())
	}
	put(t, s, msg("c", "z", 150), true)
	if count() != 1 {
		t.Fatalf("same second, larger id is unread: %d", count())
	}
	put(t, s, msg("c", "old", 149), true)
	put(t, s, msg("c", "new", 151), true)
	if count() != 2 {
		t.Fatalf("older read, newer unread: %d", count())
	}
}

func TestUnreadIncomingCursorWalksSameSecondMessages(t *testing.T) {
	s := open(t)
	ensure(t, s, "c", "", api.KindDM)
	for i := 0; i < 250; i++ {
		put(t, s, msg("c", fmt.Sprintf("u%03d", i), 500), false)
	}
	put(t, s, msg("c", "other", 400), false)
	var all []Msg
	var cur *Mark
	pages := 0
	for {
		page, err := s.UnreadIncoming(ctx, "c", Mark{}, 1<<62, cur, 100)
		if err != nil {
			t.Fatal(err)
		}
		pages++
		all = append(all, page...)
		if len(page) < 100 {
			break
		}
		last := page[len(page)-1]
		cur = &Mark{TS: last.TS, ID: last.ID}
	}
	seen := map[string]bool{}
	for _, m := range all {
		seen[m.ID] = true
	}
	if len(all) != 251 || len(seen) != 251 || pages != 3 || all[0].ID != "u249" || all[250].ID != "other" {
		t.Fatalf("walked %d rows (%d distinct) in %d pages, first %q last %q", len(all), len(seen), pages, all[0].ID, all[len(all)-1].ID)
	}
	// rows at or below the watermark and above the upper bound are excluded
	page, _ := s.UnreadIncoming(ctx, "c", Mark{TS: 500, ID: "u100"}, 500, nil, 1000)
	if len(page) != 149 || page[len(page)-1].ID != "u101" {
		t.Fatalf("after the (500, u100) mark: %d rows, last %q", len(page), page[len(page)-1].ID)
	}
	if page, _ = s.UnreadIncoming(ctx, "c", Mark{}, 450, nil, 10); len(page) != 1 || page[0].ID != "other" {
		t.Fatalf("upper bound: %+v", page)
	}
}

func TestAdminsRoundTripAndLookup(t *testing.T) {
	s := open(t)
	ensure(t, s, "g@g.us", "", api.KindGroup)
	if c, _, _ := s.GetChat(ctx, "g@g.us"); c.Admins != nil || c.HasAdmin("a@s.whatsapp.net") {
		t.Fatalf("new chat has no admins: %+v", c)
	}
	s.UpdateChat(ctx, "g@g.us", func(c *Chat) { c.Admins = []string{"a@s.whatsapp.net", "1@lid"} })
	c, _, _ := s.GetChat(ctx, "g@g.us")
	if !reflect.DeepEqual(c.Admins, []string{"a@s.whatsapp.net", "1@lid"}) || !c.HasAdmin("x", "1@lid") || c.HasAdmin("b@s.whatsapp.net") || c.HasAdmin("") {
		t.Fatalf("admins %+v", c.Admins)
	}
	s.UpdateChat(ctx, "g@g.us", func(c *Chat) { c.Admins = nil })
	if c, _, _ = s.GetChat(ctx, "g@g.us"); c.Admins != nil {
		t.Fatalf("cleared admins %+v", c.Admins)
	}
}

func TestMergeChatKeepsTheNewerReadMark(t *testing.T) {
	s := open(t)
	ensure(t, s, "lid1@lid", "", api.KindDM)
	ensure(t, s, "pn1@s.whatsapp.net", "", api.KindDM)
	s.UpdateChat(ctx, "lid1@lid", func(c *Chat) { c.ReadUpToTS, c.ReadUpToID = 40, "b"; c.Admins = []string{"x"} })
	s.UpdateChat(ctx, "pn1@s.whatsapp.net", func(c *Chat) { c.ReadUpToTS, c.ReadUpToID = 40, "a" })
	if err := s.MergeChat(ctx, "lid1@lid", "pn1@s.whatsapp.net"); err != nil {
		t.Fatal(err)
	}
	c, _, _ := s.GetChat(ctx, "pn1@s.whatsapp.net")
	if c.ReadUpToTS != 40 || c.ReadUpToID != "b" || !c.HasAdmin("x") {
		t.Fatalf("merged mark %d %q admins %v", c.ReadUpToTS, c.ReadUpToID, c.Admins)
	}
}

func TestResetLeavesNoResidueInTheFile(t *testing.T) {
	path := filepath.Join(t.TempDir(), "messages.db")
	s, err := Open(path)
	if err != nil {
		t.Fatal(err)
	}
	ensure(t, s, "c", "", api.KindDM)
	marker := "RESIDUE-" + strings.Repeat("secretmessagetext", 40)
	for i := 0; i < 50; i++ {
		m := msg("c", fmt.Sprintf("m%d", i), int64(i))
		m.Text = sp(marker)
		put(t, s, m, false)
	}
	s.db.Exec("PRAGMA wal_checkpoint(TRUNCATE)")
	if raw, _ := os.ReadFile(path); !strings.Contains(string(raw), marker) {
		t.Fatal("setup: the marker must be in the file before the reset")
	}
	if err := s.Reset(ctx); err != nil {
		t.Fatal(err)
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	for _, suffix := range []string{"", "-wal"} {
		raw, err := os.ReadFile(path + suffix)
		if err != nil && !errors.Is(err, os.ErrNotExist) {
			t.Fatal(err)
		}
		if strings.Contains(string(raw), "secretmessagetext") {
			t.Fatalf("deleted message text is still in %s%s", filepath.Base(path), suffix)
		}
	}
}
