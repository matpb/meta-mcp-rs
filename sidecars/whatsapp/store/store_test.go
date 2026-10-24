package store

import (
	"context"
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

func put(t *testing.T, s *Store, m Msg, unread bool) bool {
	t.Helper()
	ins, err := s.PutMessage(ctx, m, unread)
	if err != nil {
		t.Fatal(err)
	}
	return ins
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
		if fi.Mode().Perm() != 0o600 {
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
	if err := s.db.QueryRow("PRAGMA user_version").Scan(&ver); err != nil || ver != 1 {
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
	ms, err := s.UnreadIncoming(ctx, "c", 10, 50, 100)
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
	ms, _ = s.UnreadIncoming(ctx, "c", 0, 1<<62, 2)
	if len(ms) != 2 || ms[0].ID != "m6" {
		t.Fatalf("max/newest first: %+v", ms)
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
