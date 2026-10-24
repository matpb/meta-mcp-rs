package normalize

import (
	"context"

	"go.mau.fi/whatsmeow/types"
)

// Filtered reports chats the sidecar never stores: status, broadcast lists, newsletters, bots and similar.
func Filtered(chat types.JID) bool {
	switch chat.Server {
	case types.BroadcastServer, types.NewsletterServer, types.BotServer,
		types.HostedServer, types.HostedLIDServer, types.MessengerServer, types.InteropServer:
		return true
	}
	return false
}

// Resolver looks up the LID and phone-number forms of one account.
type Resolver interface {
	PNForLID(ctx context.Context, lid types.JID) (types.JID, bool)
	LIDForPN(ctx context.Context, pn types.JID) (types.JID, bool)
}

// Canonical returns the canonical id (phone-number form when known) and the other form ("" unknown).
func Canonical(ctx context.Context, jid, alt types.JID, r Resolver) (id, altID string) {
	jid, alt = jid.ToNonAD(), alt.ToNonAD()
	switch jid.Server {
	case types.GroupServer:
		return jid.String(), ""
	case types.DefaultUserServer:
		if alt.Server == types.HiddenUserServer && alt.User != "" {
			return jid.String(), alt.String()
		}
		if r != nil {
			if lid, ok := r.LIDForPN(ctx, jid); ok && lid.Server == types.HiddenUserServer && lid.User != "" {
				return jid.String(), lid.ToNonAD().String()
			}
		}
		return jid.String(), ""
	case types.HiddenUserServer:
		if alt.Server == types.DefaultUserServer && alt.User != "" {
			return alt.String(), jid.String()
		}
		if r != nil {
			if pn, ok := r.PNForLID(ctx, jid); ok && pn.Server == types.DefaultUserServer && pn.User != "" {
				return pn.ToNonAD().String(), jid.String()
			}
		}
		return jid.String(), ""
	}
	return jid.String(), ""
}
