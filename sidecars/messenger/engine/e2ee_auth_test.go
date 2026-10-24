package engine

import (
	"testing"

	"go.mau.fi/whatsmeow/proto/waCommon"
	"go.mau.fi/whatsmeow/proto/waConsumerApplication"
	"go.mau.fi/whatsmeow/types"
	"google.golang.org/protobuf/proto"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
	"github.com/matpb/meta-mcp-rs/sidecars/messenger/e2ee"
)

// e2eeFixture is a session with an open E2EE store, the handler whatsmeow would call and the known threads.
type e2eeFixture struct {
	s   *session
	rec *recorder
	st  *e2ee.Store
	h   func(any) bool
}

func newE2EEFixture(t *testing.T) *e2eeFixture {
	t.Helper()
	fc := newFakeClient(nil)
	s, rec := openTest(t, fc)
	s.processTable(threadsTable(), modeInitial)
	st, err := s.ensureStore(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	return &e2eeFixture{s: s, rec: rec, st: st, h: s.e2eeHandler(&cycle{})}
}

func (f *e2eeFixture) must(t *testing.T, evt any) {
	t.Helper()
	if !f.h(evt) {
		t.Fatal("event was not acked")
	}
}

func (f *e2eeFixture) get(t *testing.T, id string) (api.Message, bool) {
	t.Helper()
	m, found, err := f.st.Get(t.Context(), id)
	if err != nil {
		t.Fatal(err)
	}
	return m, found
}

func (f *e2eeFixture) messages() int { return len(f.rec.named(api.EvMessage)) }

func key(id string, fromMe bool, participant, remote string) *waCommon.MessageKey {
	k := &waCommon.MessageKey{ID: proto.String(id), FromMe: proto.Bool(fromMe)}
	if participant != "" {
		k.Participant = proto.String(participant)
	}
	if remote != "" {
		k.RemoteJID = proto.String(remote)
	}
	return k
}

func reactionApp(k *waCommon.MessageKey, emoji string) *waConsumerApplication.ConsumerApplication {
	return e2eeReactionApp(k, emoji, 1700000300000)
}

// M1: the sender of a message may edit it, from this device or another one of the same account.
func TestE2EEEditFromTheMessageSenderIsApplied(t *testing.T) {
	f := newE2EEFixture(t)
	bobJ, me := jid("333", types.MessengerServer), jid("111", types.MessengerServer)
	f.must(t, fbEvent(bobJ, bobJ, false, "B1", 1700000000, e2eeTextApp("hi")))
	f.must(t, fbEvent(bobJ, me, true, "M1", 1700000050, e2eeTextApp("mine")))
	before := f.messages()

	f.must(t, fbEvent(bobJ, bobJ, false, "E1", 1700000100, editApp(key("B1", true, "", bobJ.String()), "hello")))
	m, _ := f.get(t, "wa:333@msgr:333@msgr:B1")
	if m.Text == nil || *m.Text != "hello" || !m.Edited {
		t.Fatalf("Bob's edit of his own message: %+v", m)
	}

	otherDevice := types.JID{User: "111", Device: 3, Server: types.MessengerServer}
	f.must(t, fbEvent(bobJ, otherDevice, true, "E2", 1700000200, editApp(key("M1", true, "", bobJ.String()), "mine, fixed")))
	m, _ = f.get(t, "wa:333@msgr:111@msgr:M1")
	if m.Text == nil || *m.Text != "mine, fixed" || !m.Edited {
		t.Fatalf("an edit from another device of the owner: %+v", m)
	}
	if got := f.messages() - before; got != 2 {
		t.Fatalf("%d message events for two applied edits", got)
	}
}

// M1: an edit or unsend aimed at a message the store does not have is only surfaced when its sender wrote it.
func TestE2EEChangeOfUnknownMessageNeedsTheMatchingSender(t *testing.T) {
	f := newE2EEFixture(t)
	bobJ := jid("333", types.MessengerServer)
	grp := jid("9000", types.GroupServer)
	alice, mallory := jid("222", types.MessengerServer), jid("666", types.MessengerServer)

	f.must(t, fbEvent(bobJ, bobJ, false, "E1", 1700000100, editApp(key("GONE", true, "", bobJ.String()), "edited text")))
	evs := f.rec.named(api.EvMessage)
	if len(evs) != 1 {
		t.Fatalf("a self edit of an unknown message must be surfaced once, got %d", len(evs))
	}
	m := evs[0].data.(api.MessageEvent).Message
	if m.ID != "wa:333@msgr:333@msgr:GONE" || !m.Edited || m.Text == nil || *m.Text != "edited text" {
		t.Fatalf("surfaced message %+v", m)
	}
	f.must(t, fbEvent(bobJ, bobJ, false, "R1", 1700000200, revokeApp(key("GONE2", true, "", bobJ.String()))))
	if got := f.messages(); got != 2 || !f.rec.named(api.EvMessage)[1].data.(api.MessageEvent).Message.Unsent {
		t.Fatalf("a self unsend of an unknown message must be surfaced, got %d events", got)
	}

	before := f.messages()
	f.must(t, fbEvent(grp, mallory, false, "E9", 1700000300, editApp(key("A1", false, alice.String(), grp.String()), "forged")))
	f.must(t, fbEvent(grp, mallory, false, "R9", 1700000400, revokeApp(key("A1", false, alice.String(), grp.String()))))
	f.must(t, fbEvent(bobJ, bobJ, false, "E8", 1700000500, editApp(key("GONE", false, "", bobJ.String()), "claims to edit the owner's message")))
	if got := f.messages() - before; got != 0 {
		t.Fatalf("%d message events for forged changes of unknown messages", got)
	}
}

// M1: an unsend needs the sender, or a group admin the registry knows; an edit never takes an admin.
func TestE2EEUnsendByAdminOnlyWhenKnown(t *testing.T) {
	f := newE2EEFixture(t)
	grp := jid("9000", types.GroupServer)
	alice, mallory, carol := jid("222", types.MessengerServer), jid("666", types.MessengerServer), jid("777", types.MessengerServer)
	f.must(t, fbEvent(grp, alice, false, "A1", 1700000000, e2eeTextApp("alice says")))
	f.must(t, fbEvent(grp, alice, false, "A2", 1700000010, e2eeTextApp("alice again")))
	id1, id2 := "wa:9000@g.us:222@msgr:A1", "wa:9000@g.us:222@msgr:A2"

	revokeAlice := func(sender types.JID, evID, target string) {
		f.must(t, fbEvent(grp, sender, false, evID, 1700000100, revokeApp(key(target, false, alice.String(), grp.String()))))
	}
	revokeAlice(mallory, "R1", "A1")
	if m, _ := f.get(t, id1); m.Unsent {
		t.Fatal("a member who is not an admin unsent Alice's message")
	}

	f.s.reg.setGroupInfo("9000", "Secret", []int64{111, 222, 666, 777}, []int64{777})
	revokeAlice(mallory, "R2", "A1")
	if m, _ := f.get(t, id1); m.Unsent {
		t.Fatal("a member the registry does not know as admin unsent Alice's message")
	}

	f.must(t, fbEvent(grp, carol, false, "E1", 1700000150, editApp(key("A1", false, alice.String(), grp.String()), "admin rewrite")))
	if m, _ := f.get(t, id1); m.Edited || *m.Text != "alice says" {
		t.Fatalf("an admin edited somebody else's message: %+v", m)
	}

	revokeAlice(carol, "R3", "A1")
	m, _ := f.get(t, id1)
	if !m.Unsent || m.Text != nil {
		t.Fatalf("a known admin's unsend was refused: %+v", m)
	}
	f.must(t, fbEvent(grp, alice, false, "R4", 1700000200, revokeApp(key("A2", true, "", grp.String()))))
	if m, _ := f.get(t, id2); !m.Unsent {
		t.Fatalf("the sender's own unsend was refused: %+v", m)
	}
}

// M1: a key that points into another group than the one the event arrived in resolves to nothing.
func TestE2EEKeyIntoAnotherGroupIsIgnored(t *testing.T) {
	f := newE2EEFixture(t)
	g1, g2 := jid("9000", types.GroupServer), jid("9001", types.GroupServer)
	alice, mallory := jid("222", types.MessengerServer), jid("666", types.MessengerServer)
	f.must(t, fbEvent(g2, alice, false, "A1", 1700000000, e2eeTextApp("in group two")))
	id := "wa:9001@g.us:222@msgr:A1"
	before := f.messages()
	reactions := len(f.rec.named(api.EvReaction))

	f.must(t, fbEvent(g1, mallory, false, "E1", 1700000100, editApp(key("A1", false, alice.String(), g2.String()), "forged")))
	f.must(t, fbEvent(g1, mallory, false, "R1", 1700000200, revokeApp(key("A1", false, alice.String(), g2.String()))))
	f.must(t, fbEvent(g1, mallory, false, "X1", 1700000300, reactionApp(key("A1", false, alice.String(), g2.String()), "x")))
	m, _ := f.get(t, id)
	if m.Edited || m.Unsent || len(m.Reactions) != 0 {
		t.Fatalf("a key into another group changed the message: %+v", m)
	}
	if f.messages() != before || len(f.rec.named(api.EvReaction)) != reactions {
		t.Fatal("a key into another group produced events")
	}

	f.must(t, fbEvent(g2, alice, false, "X2", 1700000400, reactionApp(key("A1", true, "", g2.String()), "x")))
	if m, _ := f.get(t, id); len(m.Reactions) != 1 {
		t.Fatalf("a reaction from inside the group works: %+v", m)
	}
}

// Messages are stored under (chat, sender, id): a peer reusing an id lands on its own row,
// and a redelivery with other content never overwrites the stored message.
func TestE2EEMessagesAreKeyedByChatSenderAndID(t *testing.T) {
	f := newE2EEFixture(t)
	grp := jid("9000", types.GroupServer)
	alice, mallory := jid("222", types.MessengerServer), jid("666", types.MessengerServer)
	f.must(t, fbEvent(grp, alice, false, "SAME", 1700000000, e2eeTextApp("alice's text")))
	f.must(t, fbEvent(grp, mallory, false, "SAME", 1700000010, e2eeTextApp("mallory's text")))
	a, foundA := f.get(t, "wa:9000@g.us:222@msgr:SAME")
	m, foundM := f.get(t, "wa:9000@g.us:666@msgr:SAME")
	if !foundA || !foundM || *a.Text != "alice's text" || *m.Text != "mallory's text" || a.Sender.ID != "222" || m.Sender.ID != "666" {
		t.Fatalf("rows %+v / %+v", a, m)
	}

	f.must(t, fbEvent(grp, alice, false, "SAME", 1700000020, e2eeTextApp("alice rewrites by redelivery")))
	again, _ := f.get(t, "wa:9000@g.us:222@msgr:SAME")
	if *again.Text != "alice's text" || again.Edited {
		t.Fatalf("a redelivery overwrote the stored message: %+v", again)
	}
	evs := f.rec.named(api.EvMessage)
	if last := evs[len(evs)-1].data.(api.MessageEvent).Message; *last.Text != "alice's text" {
		t.Fatalf("the redelivery must re-emit the stored message, got %q", *last.Text)
	}
}
