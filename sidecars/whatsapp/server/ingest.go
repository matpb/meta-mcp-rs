package server

import (
	"context"
	"reflect"
	"slices"
	"sort"
	"strings"
	"sync"
	"time"

	"go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	"google.golang.org/protobuf/proto"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/normalize"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/store"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/wa"
)

// resolver answers LID and phone-number lookups from the store cache, then from the client.
type resolver struct {
	s  *server
	cl wa.Client
}

func (r resolver) PNForLID(ctx context.Context, lid types.JID) (types.JID, bool) {
	lid = lid.ToNonAD()
	if pn, found, err := r.s.store.PNForLID(ctx, lid.String()); err == nil && found {
		if j, err := types.ParseJID(pn); err == nil {
			return j, true
		}
	}
	pn, err := r.cl.GetPNForLID(ctx, lid)
	if err != nil || pn.IsEmpty() {
		return types.EmptyJID, false
	}
	pn = pn.ToNonAD()
	_ = r.s.store.PutLIDMapping(ctx, lid.String(), pn.String())
	return pn, true
}

func (r resolver) LIDForPN(ctx context.Context, pn types.JID) (types.JID, bool) {
	pn = pn.ToNonAD()
	if lid, found, err := r.s.store.LIDForPN(ctx, pn.String()); err == nil && found {
		if j, err := types.ParseJID(lid); err == nil {
			return j, true
		}
	}
	lid, err := r.cl.GetLIDForPN(ctx, pn)
	if err != nil || lid.IsEmpty() {
		return types.EmptyJID, false
	}
	lid = lid.ToNonAD()
	_ = r.s.store.PutLIDMapping(ctx, lid.String(), pn.String())
	return lid, true
}

// canon returns the canonical id and the other form of a user or chat JID, learning LID pairings on the way.
func (s *server) canon(ctx context.Context, cl wa.Client, jid, alt types.JID) (string, string, error) {
	id, altID := normalize.Canonical(ctx, jid, alt, resolver{s, cl})
	if altID == "" || !strings.HasSuffix(id, "@"+types.DefaultUserServer) || !strings.HasSuffix(altID, "@"+types.HiddenUserServer) {
		return id, altID, nil
	}
	if err := s.store.PutLIDMapping(ctx, altID, id); err != nil {
		return "", "", err
	}
	if c, found, err := s.store.GetChat(ctx, altID); err != nil {
		return "", "", err
	} else if found && c.JID == altID {
		if err := s.store.MergeChat(ctx, altID, id); err != nil {
			return "", "", err
		}
		// the daemon still knows the chat by its LID id; tell it the thread moved to the phone-number id
		merged, ok, err := s.store.GetChat(ctx, id)
		if err != nil {
			return "", "", err
		}
		if ok {
			if err := s.emit(api.EvThread, api.ThreadEvent{Thread: merged.API()}); err != nil {
				return "", "", err
			}
		}
	}
	return id, altID, nil
}

func chatKind(jid types.JID) string {
	if jid.Server == types.GroupServer {
		return api.KindGroup
	}
	return api.KindDM
}

// origin describes where a message came from after canonicalisation.
type origin struct {
	info       types.MessageInfo
	chatID     string
	chatAlt    string
	kind       string
	senderID   string
	senderAlt  string
	senderName string
	self       wa.Self
}

func (o origin) direction() string {
	if o.info.IsFromMe {
		return "out"
	}
	return "in"
}

// senderForms lists the ids the sender of an event goes by.
func (o origin) senderForms() []string {
	return []string{o.senderID, o.senderAlt, o.info.Sender.ToNonAD().String()}
}

// originOf canonicalises the chat and sender of info; chatID and chatAlt are used when already known.
func (s *server) originOf(ctx context.Context, cl wa.Client, info types.MessageInfo, chatID, chatAlt string) (origin, error) {
	o := origin{info: info, chatID: chatID, chatAlt: chatAlt, kind: chatKind(info.Chat), self: cl.Self()}
	if o.chatID == "" {
		alt := types.EmptyJID
		if o.kind == api.KindDM {
			alt = info.SenderAlt
			if info.IsFromMe {
				alt = info.RecipientAlt
			}
		}
		var err error
		if o.chatID, o.chatAlt, err = s.canon(ctx, cl, info.Chat, alt); err != nil {
			return o, err
		}
	}
	if info.IsFromMe && !o.self.JID.IsEmpty() {
		o.senderID = o.self.JID.String()
		if !o.self.LID.IsEmpty() {
			o.senderAlt = o.self.LID.String()
		}
		o.senderName = o.self.PushName
		return o, nil
	}
	var err error
	if o.senderID, o.senderAlt, err = s.canon(ctx, cl, info.Sender, info.SenderAlt); err != nil {
		return o, err
	}
	if info.PushName != "" {
		for _, id := range []string{o.senderID, o.senderAlt} {
			if id == "" {
				continue
			}
			if err := s.store.PutPushName(ctx, id, info.PushName); err != nil {
				return o, err
			}
		}
	}
	o.senderName, err = s.nameOf(ctx, o.senderID, o.senderAlt)
	if err != nil {
		return o, err
	}
	if o.senderName == "" {
		o.senderName = info.PushName
	}
	return o, nil
}

// nameOf returns the best stored contact name among the given ids.
func (s *server) nameOf(ctx context.Context, ids ...string) (string, error) {
	for _, id := range ids {
		if id == "" {
			continue
		}
		n, found, err := s.store.ContactName(ctx, id)
		if err != nil {
			return "", err
		}
		if found {
			return n, nil
		}
	}
	return "", nil
}

// msgFrom builds the stored form of a message from its origin and content.
func (s *server) msgFrom(o origin, chatKey string, content *waE2E.Message) store.Msg {
	c := normalize.Extract(content)
	raw, _ := proto.Marshal(content)
	return store.Msg{
		Chat: chatKey, ID: o.info.ID,
		ChatWire: o.info.Chat.ToNonAD().String(), SenderWire: o.info.Sender.ToNonAD().String(),
		Sender: o.senderID, SenderAlt: o.senderAlt, SenderName: o.senderName,
		TS: o.info.Timestamp.Unix(), FromMe: o.info.IsFromMe,
		Kind: c.Kind, Text: c.Text, ReplyTo: c.ReplyTo, Attachments: c.Attachments, Raw: raw,
	}
}

// ensureChat creates the chat on first sight and fills DM participants and name.
func (s *server) ensureChat(ctx context.Context, cl wa.Client, o origin) (store.Chat, bool, error) {
	chat, created, err := s.store.EnsureChat(ctx, o.chatID, o.chatAlt, o.kind)
	if err != nil {
		return chat, false, err
	}
	if o.kind != api.KindDM {
		return chat, created, nil
	}
	peerName := ""
	if o.info.IsFromMe {
		if peerName, err = s.nameOf(ctx, o.chatID, o.chatAlt); err != nil {
			return chat, created, err
		}
	} else {
		peerName = o.senderName
	}
	if !created && (chat.Name != "" || peerName == "") {
		return chat, created, nil
	}
	chat, _, err = s.store.UpdateChat(ctx, chat.JID, func(c *store.Chat) {
		if c.Name == "" {
			c.Name = peerName
		}
		if c.Participants == nil {
			c.Participants = dmParticipants(c.JID, c.Name, o.self)
		}
	})
	return chat, created, err
}

func dmParticipants(peerID, peerName string, self wa.Self) []api.Participant {
	ps := []api.Participant{{ID: peerID, Name: peerName}}
	if !self.JID.IsEmpty() {
		ps = append(ps, api.Participant{ID: self.JID.String(), Name: self.PushName, IsMe: true})
	}
	return ps
}

// findChat resolves a JID to a stored chat by its canonical or alternate id.
func (s *server) findChat(ctx context.Context, cl wa.Client, jid types.JID) (store.Chat, bool, error) {
	id, alt, err := s.canon(ctx, cl, jid, types.EmptyJID)
	if err != nil {
		return store.Chat{}, false, err
	}
	chat, found, err := s.store.GetChat(ctx, id)
	if err != nil || found || alt == "" {
		return chat, found, err
	}
	return s.store.GetChat(ctx, alt)
}

// onMessage stores and reports one live message, edit, revoke or reaction.
func (s *server) onMessage(ctx context.Context, cl wa.Client, e *events.Message) error {
	if normalize.Filtered(e.Info.Chat) {
		return nil
	}
	s.ingestMu.Lock()
	defer s.ingestMu.Unlock()
	cls := normalize.Classify(e.Message)
	if cls.Action == normalize.ActIgnore {
		return nil
	}
	o, err := s.originOf(ctx, cl, e.Info, "", "")
	if err != nil {
		return err
	}
	switch cls.Action {
	case normalize.ActMessage:
		return s.liveMessage(ctx, cl, o, cls.Content)
	case normalize.ActEdit:
		return s.applyEdit(ctx, cl, o, cls.TargetID, cls.Content)
	case normalize.ActRevoke:
		return s.applyRevoke(ctx, cl, o, cls.TargetID)
	case normalize.ActReaction:
		return s.applyReaction(ctx, cl, o, cls.TargetID, cls.Emoji)
	case normalize.ActEncryptedReaction:
		dec, err := cl.DecryptReaction(ctx, e)
		if err != nil {
			s.log.Info().Msg("could not decrypt a reaction")
			return nil
		}
		return s.applyReaction(ctx, cl, o, cls.TargetID, dec.GetText())
	case normalize.ActEncryptedEdit:
		dec, err := cl.DecryptSecretEncryptedMessage(ctx, e)
		if err != nil || dec == nil {
			s.log.Info().Msg("could not decrypt an edit")
			return nil
		}
		return s.applyEdit(ctx, cl, o, cls.TargetID, unwrapEdit(dec))
	}
	return nil
}

// unwrapEdit returns the new content inside a decrypted edit payload.
func unwrapEdit(m *waE2E.Message) *waE2E.Message {
	if c := normalize.Classify(m); c.Action == normalize.ActEdit {
		return c.Content
	}
	if inner := m.GetEditedMessage().GetMessage(); inner != nil {
		return inner
	}
	return m
}

func (s *server) liveMessage(ctx context.Context, cl wa.Client, o origin, content *waE2E.Message) error {
	chat, created, err := s.ensureChat(ctx, cl, o)
	if err != nil {
		return err
	}
	m := s.msgFrom(o, chat.JID, content)
	res, err := s.store.PutMessage(ctx, m, true)
	if err != nil {
		return err
	}
	if res == store.PutConflict {
		s.log.Info().Str("thread", chat.JID).Str("id", m.ID).Msg("ignored a message that reuses a stored id")
		return nil
	}
	if created {
		if chat, _, err = s.store.GetChat(ctx, chat.JID); err != nil {
			return err
		}
		if err := s.emit(api.EvThread, api.ThreadEvent{Thread: chat.API()}); err != nil {
			return err
		}
	}
	stored, found, err := s.store.GetMessage(ctx, chat.JID, m.ID)
	if err != nil {
		return err
	}
	if !found {
		stored = m
	}
	if err := s.emit(api.EvMessage, api.MessageEvent{Message: stored.API(), Direction: o.direction()}); err != nil {
		return err
	}
	s.maybeFetchGroup(cl, chat)
	return nil
}

// mayChange reports whether the sender of o may edit or revoke the stored message m of chat.
// Only the author may edit; a revoke also comes from a known admin of the group.
func mayChange(chat store.Chat, m store.Msg, o origin, revoke bool) bool {
	if m.WrittenBy(o.info.IsFromMe, o.senderForms()...) {
		return true
	}
	return revoke && chat.Kind == api.KindGroup && chat.HasAdmin(o.senderForms()...)
}

// ignoredChange logs, with ids only, an edit or revoke that its sender had no right to make.
func (s *server) ignoredChange(op string, chat store.Chat, target string) {
	s.log.Info().Str("op", op).Str("thread", chat.JID).Str("id", target).Msg("ignored a change from a sender that does not own the message")
}

func (s *server) applyEdit(ctx context.Context, cl wa.Client, o origin, target string, content *waE2E.Message) error {
	chat, found, err := s.store.GetChat(ctx, o.chatID)
	if err != nil || !found || content == nil {
		return err
	}
	c := normalize.Extract(content)
	raw, _ := proto.Marshal(content)
	m, res, err := s.store.UpdateMessageIf(ctx, chat.JID, target, func(m store.Msg) bool { return mayChange(chat, m, o, false) }, func(m *store.Msg) {
		m.Kind, m.Text, m.Attachments, m.Raw, m.Edited = c.Kind, c.Text, c.Attachments, raw, true
		if c.ReplyTo != nil {
			m.ReplyTo = c.ReplyTo
		}
	})
	if err != nil {
		return err
	}
	if res == store.UpdateDenied {
		s.ignoredChange("edit", chat, target)
	}
	if res != store.UpdateDone {
		return nil
	}
	return s.emit(api.EvMessage, api.MessageEvent{Message: m.API(), Direction: o.direction()})
}

// revoke unsends the target message when the sender of o may; it reports whether the row changed.
func (s *server) revoke(ctx context.Context, chat store.Chat, o origin, target string) (store.Msg, bool, error) {
	m, res, err := s.store.UpdateMessageIf(ctx, chat.JID, target, func(m store.Msg) bool { return mayChange(chat, m, o, true) }, func(m *store.Msg) {
		m.Unsent, m.Text, m.Attachments, m.Raw = true, nil, []api.Attachment{}, nil
	})
	if err != nil {
		return store.Msg{}, false, err
	}
	if res == store.UpdateDenied {
		s.ignoredChange("revoke", chat, target)
	}
	return m, res == store.UpdateDone, nil
}

func (s *server) applyRevoke(ctx context.Context, cl wa.Client, o origin, target string) error {
	chat, found, err := s.store.GetChat(ctx, o.chatID)
	if err != nil || !found {
		return err
	}
	m, changed, err := s.revoke(ctx, chat, o, target)
	if err != nil || !changed {
		return err
	}
	return s.emit(api.EvMessage, api.MessageEvent{Message: m.API(), Direction: o.direction()})
}

func (s *server) applyReaction(ctx context.Context, cl wa.Client, o origin, target, emoji string) error {
	chat, found, err := s.store.GetChat(ctx, o.chatID)
	if err != nil || !found || target == "" {
		return err
	}
	ts := o.info.Timestamp.Unix()
	if err := s.store.PutReaction(ctx, chat.JID, target, o.senderID, emoji, ts); err != nil {
		return err
	}
	ev := api.ReactionEvent{ThreadID: chat.JID, MessageID: target, SenderID: o.senderID, TS: ts}
	if emoji != "" {
		ev.Emoji = &emoji
	}
	return s.emit(api.EvReaction, ev)
}

// onReceipt records delivery and read receipts and the owner's own reads on other devices.
func (s *server) onReceipt(ctx context.Context, cl wa.Client, e *events.Receipt) error {
	kind := ""
	switch e.Type {
	case types.ReceiptTypeDelivered:
		kind = "delivered"
	case types.ReceiptTypeRead, types.ReceiptTypePlayed:
		kind = "read"
	case types.ReceiptTypeReadSelf:
		kind = "read-self"
	default:
		return nil
	}
	if normalize.Filtered(e.Chat) {
		return nil
	}
	s.ingestMu.Lock()
	defer s.ingestMu.Unlock()
	alt := types.EmptyJID
	if chatKind(e.Chat) == api.KindDM {
		alt = e.SenderAlt
	}
	chatID, _, err := s.canon(ctx, cl, e.Chat, alt)
	if err != nil {
		return err
	}
	chat, found, err := s.store.GetChat(ctx, chatID)
	if err != nil || !found {
		return err
	}
	var newest store.Mark
	for _, id := range e.MessageIDs {
		m, ok, err := s.store.GetMessage(ctx, chat.JID, id)
		if err != nil {
			return err
		}
		if mark := (store.Mark{TS: m.TS, ID: m.ID}); ok && mark.After(newest) {
			newest = mark
		}
	}
	if kind == "read-self" {
		upd, _, err := s.store.UpdateChat(ctx, chat.JID, func(c *store.Chat) {
			c.UnreadCount = 0
			if newest.After(store.Mark{TS: c.ReadUpToTS, ID: c.ReadUpToID}) {
				c.ReadUpToTS, c.ReadUpToID = newest.TS, newest.ID
			}
		})
		if err != nil {
			return err
		}
		return s.emit(api.EvThread, api.ThreadEvent{Thread: upd.API()})
	}
	upTo := newest.TS
	if upTo == 0 {
		upTo = e.Timestamp.Unix()
	}
	sender, _, err := s.canon(ctx, cl, e.Sender, e.SenderAlt)
	if err != nil {
		return err
	}
	if err := s.store.PutReceipt(ctx, chat.JID, sender, kind, upTo); err != nil {
		return err
	}
	return s.emit(api.EvReceipt, api.ReceiptEvent{ThreadID: chat.JID, Kind: kind, SenderID: sender, UpToTS: upTo})
}

// emitThreadIfChanged reports a chat whose protocol view changed.
func (s *server) emitThreadIfChanged(before, after store.Chat) error {
	if reflect.DeepEqual(before.API(), after.API()) {
		return nil
	}
	return s.emit(api.EvThread, api.ThreadEvent{Thread: after.API()})
}

// updateChat applies fn to the chat of jid and reports the change.
func (s *server) updateChat(ctx context.Context, cl wa.Client, jid types.JID, fn func(*store.Chat)) error {
	chat, found, err := s.findChat(ctx, cl, jid)
	if err != nil || !found {
		return err
	}
	upd, _, err := s.store.UpdateChat(ctx, chat.JID, fn)
	if err != nil {
		return err
	}
	return s.emitThreadIfChanged(chat, upd)
}

// onAppState applies app state sync events (pin, archive, mute, read marks, contacts, group renames).
func (s *server) onAppState(ctx context.Context, cl wa.Client, evt any) error {
	s.ingestMu.Lock()
	defer s.ingestMu.Unlock()
	switch e := evt.(type) {
	case *events.Pin:
		return s.updateChat(ctx, cl, e.JID, func(c *store.Chat) { c.Pinned = e.Action.GetPinned() })
	case *events.Archive:
		return s.updateChat(ctx, cl, e.JID, func(c *store.Chat) { c.Archived = e.Action.GetArchived() })
	case *events.Mute:
		return s.updateChat(ctx, cl, e.JID, func(c *store.Chat) {
			c.MutedUntil = 0
			if e.Action.GetMuted() {
				c.MutedUntil = e.Action.GetMuteEndTimestamp()
				if c.MutedUntil <= 0 {
					c.MutedUntil = -1
				}
			}
		})
	case *events.MarkChatAsRead:
		return s.updateChat(ctx, cl, e.JID, func(c *store.Chat) {
			if e.Action.GetRead() {
				c.UnreadCount = 0
			} else if c.UnreadCount < 1 {
				c.UnreadCount = 1
			}
		})
	case *events.DeleteChat:
		chat, found, err := s.findChat(ctx, cl, e.JID)
		if err != nil || !found {
			return err
		}
		return s.store.DeleteChat(ctx, chat.JID)
	case *events.Contact:
		return s.onContact(ctx, cl, e)
	case *events.PushName:
		return s.onPushName(ctx, cl, e)
	case *events.JoinedGroup:
		return s.onJoinedGroup(ctx, cl, e)
	case *events.GroupInfo:
		return s.onGroupInfo(ctx, cl, e)
	}
	return nil
}

func (s *server) onContact(ctx context.Context, cl wa.Client, e *events.Contact) error {
	full, first := e.Action.GetFullName(), e.Action.GetFirstName()
	ids := []types.JID{e.JID}
	for _, raw := range []string{e.Action.GetPnJID(), e.Action.GetLidJID()} {
		if j, err := types.ParseJID(raw); err == nil && raw != "" {
			ids = append(ids, j)
		}
	}
	if len(ids) == 3 {
		if err := s.store.PutLIDMapping(ctx, ids[2].ToNonAD().String(), ids[1].ToNonAD().String()); err != nil {
			return err
		}
	}
	for _, j := range ids {
		if err := s.store.PutContactName(ctx, j.ToNonAD().String(), full, first); err != nil {
			return err
		}
	}
	return s.refreshName(ctx, cl, e.JID)
}

func (s *server) onPushName(ctx context.Context, cl wa.Client, e *events.PushName) error {
	for _, j := range []types.JID{e.JID, e.JIDAlt} {
		if j.IsEmpty() {
			continue
		}
		if err := s.store.PutPushName(ctx, j.ToNonAD().String(), e.NewPushName); err != nil {
			return err
		}
	}
	return s.refreshName(ctx, cl, e.JID)
}

// refreshName sets a DM chat's name from the best known contact name.
func (s *server) refreshName(ctx context.Context, cl wa.Client, jid types.JID) error {
	if jid.Server == types.GroupServer {
		return nil
	}
	chat, found, err := s.findChat(ctx, cl, jid)
	if err != nil || !found || chat.Kind != api.KindDM {
		return err
	}
	name, err := s.nameOf(ctx, chat.JID, chat.AltJID)
	if err != nil || name == "" || name == chat.Name {
		return err
	}
	upd, _, err := s.store.UpdateChat(ctx, chat.JID, func(c *store.Chat) { c.Name = name })
	if err != nil {
		return err
	}
	return s.emitThreadIfChanged(chat, upd)
}

func (s *server) onJoinedGroup(ctx context.Context, cl wa.Client, e *events.JoinedGroup) error {
	if normalize.Filtered(e.JID) || e.JID.Server != types.GroupServer {
		return nil
	}
	chat, _, err := s.store.EnsureChat(ctx, e.JID.String(), "", api.KindGroup)
	if err != nil {
		return err
	}
	parts, admins, err := s.participantsOf(ctx, cl, &e.GroupInfo)
	if err != nil {
		return err
	}
	upd, _, err := s.store.UpdateChat(ctx, chat.JID, func(c *store.Chat) {
		if e.Name != "" {
			c.Name = e.Name
		}
		c.Participants, c.GroupInfoAt, c.Admins = parts, time.Now().Unix(), admins
	})
	if err != nil {
		return err
	}
	return s.emit(api.EvThread, api.ThreadEvent{Thread: upd.API()})
}

func (s *server) onGroupInfo(ctx context.Context, cl wa.Client, e *events.GroupInfo) error {
	roles := len(e.Promote) > 0 || len(e.Demote) > 0
	refresh := len(e.Join) > 0 || len(e.Leave) > 0 || roles
	if e.Name == nil && !refresh {
		return nil
	}
	promote, err := s.formsOf(ctx, cl, e.Promote)
	if err != nil {
		return err
	}
	demote, err := s.formsOf(ctx, cl, e.Demote)
	if err != nil {
		return err
	}
	return s.updateChat(ctx, cl, e.JID, func(c *store.Chat) {
		if e.Name != nil && e.Name.Name != "" {
			c.Name = e.Name.Name
		}
		if refresh {
			c.GroupInfoAt = 0
		}
		if roles {
			c.Admins = changeAdmins(c.Admins, promote, demote)
		}
	})
}

// formsOf lists every id form of the given users.
func (s *server) formsOf(ctx context.Context, cl wa.Client, jids []types.JID) ([]string, error) {
	var out []string
	for _, j := range jids {
		id, alt, err := s.canon(ctx, cl, j, types.EmptyJID)
		if err != nil {
			return nil, err
		}
		out = addIDs(out, id, alt, j.ToNonAD().String())
	}
	return out, nil
}

// addIDs appends the ids that are not empty and not already in list.
func addIDs(list []string, ids ...string) []string {
	for _, id := range ids {
		if id != "" && !slices.Contains(list, id) {
			list = append(list, id)
		}
	}
	return list
}

// changeAdmins adds the promoted ids to admins and removes the demoted ones.
func changeAdmins(admins, promote, demote []string) []string {
	out := make([]string, 0, len(admins)+len(promote))
	for _, a := range admins {
		if !slices.Contains(demote, a) {
			out = append(out, a)
		}
	}
	return addIDs(out, promote...)
}

// participantsOf converts group info members to protocol participants and lists the ids of the admins.
func (s *server) participantsOf(ctx context.Context, cl wa.Client, gi *types.GroupInfo) ([]api.Participant, []string, error) {
	self := cl.Self()
	out := make([]api.Participant, 0, len(gi.Participants))
	var admins []string
	for _, p := range gi.Participants {
		alt := p.PhoneNumber
		if p.JID.Server == types.DefaultUserServer {
			alt = p.LID
		}
		id, altID, err := s.canon(ctx, cl, p.JID, alt)
		if err != nil {
			return nil, nil, err
		}
		name, err := s.nameOf(ctx, id, altID)
		if err != nil {
			return nil, nil, err
		}
		if name == "" {
			name = p.DisplayName
		}
		isMe := (!self.JID.IsEmpty() && id == self.JID.String()) || (!self.LID.IsEmpty() && (p.JID.User == self.LID.User || p.LID.User == self.LID.User))
		out = append(out, api.Participant{ID: id, Name: name, IsMe: isMe})
		if p.IsAdmin || p.IsSuperAdmin {
			admins = addIDs(admins, id, altID)
		}
	}
	return out, admins, nil
}

// groupFetcher paces background GetGroupInfo calls: one at a time with a minimum gap.
type groupFetcher struct {
	mu       sync.Mutex
	inflight bool
	last     time.Time
	tried    map[string]time.Time
}

// maybeFetchGroup schedules a background group info refresh when the stored one is stale.
func (s *server) maybeFetchGroup(cl wa.Client, chat store.Chat) {
	if chat.Kind != api.KindGroup {
		return
	}
	now := time.Now()
	if chat.GroupInfoAt != 0 && now.Sub(time.Unix(chat.GroupInfoAt, 0)) < s.tm.groupTTL {
		return
	}
	g := &s.groups
	g.mu.Lock()
	if g.inflight || now.Sub(g.tried[chat.JID]) < s.tm.groupRetry {
		g.mu.Unlock()
		return
	}
	g.inflight = true
	g.tried[chat.JID] = now
	g.mu.Unlock()
	s.bg.Add(1)
	go s.fetchGroup(cl, chat.JID)
}

func (s *server) fetchGroup(cl wa.Client, chatJID string) {
	defer s.bg.Done()
	g := &s.groups
	defer func() {
		g.mu.Lock()
		g.inflight = false
		g.mu.Unlock()
	}()
	defer s.recoverPanic("group")
	g.mu.Lock()
	wait := s.tm.groupGap - time.Since(g.last)
	g.mu.Unlock()
	if wait > 0 {
		select {
		case <-time.After(wait):
		case <-s.ctx.Done():
			return
		}
	}
	jid, err := types.ParseJID(chatJID)
	if err != nil {
		return
	}
	ctx, cancel := context.WithTimeout(s.ctx, 30*time.Second)
	defer cancel()
	info, err := cl.GetGroupInfo(ctx, jid)
	g.mu.Lock()
	g.last = time.Now()
	g.mu.Unlock()
	if err != nil || info == nil {
		s.log.Info().Msg("group info fetch failed")
		return
	}
	s.ingestMu.Lock()
	defer s.ingestMu.Unlock()
	parts, admins, err := s.participantsOf(ctx, cl, info)
	if err != nil {
		return
	}
	upd, found, err := s.store.UpdateChat(ctx, chatJID, func(c *store.Chat) {
		if info.Name != "" {
			c.Name = info.Name
		}
		c.Participants, c.GroupInfoAt, c.Admins = parts, time.Now().Unix(), admins
	})
	if err != nil || !found {
		return
	}
	s.emitLog(api.EvThread, api.ThreadEvent{Thread: upd.API()})
}

// sortNewestFirst orders messages by timestamp then id, newest first.
func sortNewestFirst(ms []store.Msg) {
	sort.SliceStable(ms, func(i, j int) bool {
		if ms[i].TS != ms[j].TS {
			return ms[i].TS > ms[j].TS
		}
		return ms[i].ID > ms[j].ID
	})
}
