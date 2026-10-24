package normalize

import (
	"context"
	"testing"

	"go.mau.fi/whatsmeow/types"
)

type fakeResolver struct {
	pn  map[string]types.JID // lid user -> pn
	lid map[string]types.JID // pn user -> lid
}

func (f fakeResolver) PNForLID(_ context.Context, lid types.JID) (types.JID, bool) {
	j, ok := f.pn[lid.User]
	return j, ok
}

func (f fakeResolver) LIDForPN(_ context.Context, pn types.JID) (types.JID, bool) {
	j, ok := f.lid[pn.User]
	return j, ok
}

func TestCanonical(t *testing.T) {
	pn := types.NewJID("15551234567", types.DefaultUserServer)
	lid := types.NewJID("90001", types.HiddenUserServer)
	group := types.NewJID("120363000000000001", types.GroupServer)
	res := fakeResolver{
		pn:  map[string]types.JID{"90001": pn},
		lid: map[string]types.JID{"15551234567": lid},
	}
	withDevice := func(j types.JID, d uint16) types.JID { j.Device = d; return j }

	tests := []struct {
		name     string
		jid      types.JID
		alt      types.JID
		r        Resolver
		id, alt2 string
	}{
		{"group", group, types.EmptyJID, nil, "120363000000000001@g.us", ""},
		{"group ignores alt", group, lid, res, "120363000000000001@g.us", ""},
		{"pn without resolver", pn, types.EmptyJID, nil, "15551234567@s.whatsapp.net", ""},
		{"pn with resolver lid", pn, types.EmptyJID, res, "15551234567@s.whatsapp.net", "90001@lid"},
		{"pn with resolver miss", pn, types.EmptyJID, fakeResolver{}, "15551234567@s.whatsapp.net", ""},
		{"pn with lid alt", pn, lid, nil, "15551234567@s.whatsapp.net", "90001@lid"},
		{"lid with pn alt", lid, pn, nil, "15551234567@s.whatsapp.net", "90001@lid"},
		{"lid with resolver hit", lid, types.EmptyJID, res, "15551234567@s.whatsapp.net", "90001@lid"},
		{"lid unknown", lid, types.EmptyJID, nil, "90001@lid", ""},
		{"lid resolver miss", lid, types.EmptyJID, fakeResolver{}, "90001@lid", ""},
		{"lid with lid alt is not a pn", lid, lid, nil, "90001@lid", ""},
		{"device part stripped on id and alt", withDevice(lid, 7), withDevice(pn, 3), nil, "15551234567@s.whatsapp.net", "90001@lid"},
		{"pn device stripped", withDevice(pn, 12), withDevice(lid, 4), nil, "15551234567@s.whatsapp.net", "90001@lid"},
		{"other server", types.NewJID("status", types.BroadcastServer), types.EmptyJID, res, "status@broadcast", ""},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			id, alt := Canonical(context.Background(), tc.jid, tc.alt, tc.r)
			if id != tc.id || alt != tc.alt2 {
				t.Errorf("got (%q,%q), want (%q,%q)", id, alt, tc.id, tc.alt2)
			}
		})
	}
}

func TestFiltered(t *testing.T) {
	tests := []struct {
		name string
		jid  types.JID
		want bool
	}{
		{"status broadcast", types.StatusBroadcastJID, true},
		{"broadcast list", types.NewJID("1700000000", types.BroadcastServer), true},
		{"newsletter", types.NewJID("120363", types.NewsletterServer), true},
		{"bot", types.NewJID("bot1", types.BotServer), true},
		{"hosted", types.NewJID("1", types.HostedServer), true},
		{"hosted lid", types.NewJID("1", types.HostedLIDServer), true},
		{"messenger", types.NewJID("1", types.MessengerServer), true},
		{"interop", types.NewJID("1", types.InteropServer), true},
		{"phone number", types.NewJID("15551234567", types.DefaultUserServer), false},
		{"lid", types.NewJID("90001", types.HiddenUserServer), false},
		{"group", types.NewJID("120363000000000001", types.GroupServer), false},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if got := Filtered(tc.jid); got != tc.want {
				t.Errorf("Filtered(%v) = %v, want %v", tc.jid, got, tc.want)
			}
		})
	}
}
