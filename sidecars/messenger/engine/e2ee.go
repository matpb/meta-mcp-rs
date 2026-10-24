package engine

import (
	"context"
	"errors"
	"slices"
	"strconv"
	"time"

	mtypes "go.mau.fi/mautrix-meta/pkg/messagix/types"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
	"github.com/matpb/meta-mcp-rs/sidecars/messenger/e2ee"
	"github.com/matpb/meta-mcp-rs/sidecars/messenger/normalize"
)

const (
	codeE2EEUnavailable = "e2ee_unavailable"
	codeE2EELoggedOut   = "e2ee_logged_out"
	codeE2EECAT         = "e2ee_cat_refresh"
	e2eeOpTimeout       = 30 * time.Second
)

// ensureStore opens the E2EE database on first use, bound to this account.
func (s *session) ensureStore(ctx context.Context) (*e2ee.Store, error) {
	s.e2mu.Lock()
	defer s.e2mu.Unlock()
	if s.closed.Load() {
		return nil, api.Errorf(api.CodeNotLoggedIn, "session closed")
	}
	if s.st != nil {
		return s.st, nil
	}
	st, wiped, err := e2ee.OpenFor(ctx, s.dataDir, s.fbid, s.log.With().Str("component", "e2ee").Logger())
	if err != nil {
		return nil, api.AsError(err)
	}
	if wiped {
		s.log.Warn().Msg("e2ee data of another account was wiped")
		s.setE2EERegistered(false)
	}
	s.st = st
	return st, nil
}

// e2eeStore returns the open E2EE store, or nil.
func (s *session) e2eeStore() *e2ee.Store {
	s.e2mu.Lock()
	defer s.e2mu.Unlock()
	return s.st
}

// e2eeClient returns the connected whatsmeow client, or nil.
func (s *session) e2eeClient() *whatsmeow.Client {
	s.e2mu.Lock()
	defer s.e2mu.Unlock()
	return s.wa
}

func (s *session) stopE2EE() {
	s.e2mu.Lock()
	wa := s.wa
	s.wa = nil
	s.e2mu.Unlock()
	if wa != nil {
		wa.Disconnect()
	}
}

func (s *session) setE2EERegistered(v bool) {
	s.mu.Lock()
	changed := s.e2eeRegistered != v
	s.e2eeRegistered = v
	s.mu.Unlock()
	if changed {
		s.watch.check()
	}
}

func (s *session) e2eeActive() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.e2eeRegistered
}

// startE2EE runs once per connect cycle; a fail-closed verdict logs the account out, other failures leave the unencrypted side untouched.
// It does nothing after a fatal account signal, and a panic inside is contained.
func (s *session) startE2EE(cyc *cycle) {
	if s.blockedReason() != "" {
		return
	}
	s.guard("e2ee start", func() {
		err := s.connectE2EE(cyc)
		if err == nil || cyc.ctx.Err() != nil {
			return
		}
		if f := classify(err); f.kind == fatalLoggedOut {
			s.failClosed(f)
			return
		}
		s.log.Warn().Msg("e2ee connect failed")
		s.emit(api.EvError, api.ErrorEvent{Code: codeE2EEUnavailable, Message: "encrypted chats are unavailable"})
	})
}

func (s *session) connectE2EE(cyc *cycle) error {
	ctx := cyc.ctx
	st, err := s.ensureStore(ctx)
	if err != nil {
		return err
	}
	dev, err := st.Container().GetFirstDevice(ctx)
	if err != nil {
		return err
	}
	cl := cyc.client
	cl.SetDevice(dev)
	if dev.ID == nil {
		s.log.Info().Msg("registering e2ee device")
		if err := cl.RegisterE2EE(ctx, s.me.Load()); err != nil {
			return err
		}
		if err := dev.Save(ctx); err != nil {
			return err
		}
	}
	s.setE2EERegistered(true)
	wa, err := cl.PrepareE2EEClient()
	if err != nil {
		return err
	}
	wa.SynchronousAck = true
	wa.EnableDecryptedEventBuffer = true
	wa.AddEventHandlerWithSuccessStatus(s.e2eeHandler(cyc))
	s.e2mu.Lock()
	if ctx.Err() != nil {
		s.e2mu.Unlock()
		return ctx.Err()
	}
	s.wa = wa
	s.e2mu.Unlock()
	if err := wa.Connect(); err != nil {
		s.e2mu.Lock()
		if s.wa == wa {
			s.wa = nil
		}
		s.e2mu.Unlock()
		return err
	}
	if s.e2eeStale(ctx, wa) {
		wa.Disconnect()
	}
	return nil
}

// e2eeStale reports that wa was replaced, its cycle ended or sending got blocked while it was connecting.
func (s *session) e2eeStale(ctx context.Context, wa *whatsmeow.Client) bool {
	s.e2mu.Lock()
	defer s.e2mu.Unlock()
	return s.wa != wa || ctx.Err() != nil || s.blockedReason() != ""
}

// e2eeHandler returns whatsmeow's handler; false (no ack) asks for redelivery, also after a panic.
func (s *session) e2eeHandler(cyc *cycle) func(evt any) bool {
	return func(raw any) (ok bool) {
		defer func() {
			if r := recover(); r != nil {
				s.log.Error().Str("op", "e2ee handler").Msg("panic contained")
				s.emit(api.EvError, api.ErrorEvent{Code: api.CodeInternal, Message: "an internal error was contained"})
				ok = false
			}
		}()
		switch evt := raw.(type) {
		case *events.FBMessage:
			return s.onFBMessage(evt)
		case *events.Receipt:
			s.onReceipt(evt)
		case *events.ConnectFailure:
			s.onConnectFailure(cyc, evt)
		case *events.LoggedOut:
			s.emit(api.EvError, api.ErrorEvent{Code: codeE2EELoggedOut, Message: "the encrypted chat session was logged out"})
			s.setE2EERegistered(false)
			s.onE2EEFatal("device_removed")
		case *events.GroupInfo:
			s.onGroupInfo(evt)
		case *events.TemporaryBan:
			s.onE2EEFatal("temporary_ban")
		case *events.StreamReplaced:
			s.onE2EEFatal("stream_replaced")
		case *events.ClientOutdated:
			s.onE2EEFatal("client_outdated")
		case *events.CATRefreshError:
			s.onCATError(evt)
		}
		return true
	}
}

// onGroupInfo keeps the admin list of an encrypted group current from promotions, demotions and departures.
func (s *session) onGroupInfo(evt *events.GroupInfo) {
	if len(evt.Promote) == 0 && len(evt.Demote) == 0 && len(evt.Leave) == 0 {
		return
	}
	gone := append(slices.Clone(evt.Demote), evt.Leave...)
	s.reg.updateAdmins(normalize.ThreadIDFromJID(evt.JID), userIDs(evt.Promote), userIDs(gone))
}

// userIDs lists the numeric user ids of JIDs, skipping any that are not numeric.
func userIDs(jids []types.JID) []int64 {
	out := make([]int64, 0, len(jids))
	for _, j := range jids {
		if n := int64(j.UserInt()); n != 0 {
			out = append(out, n)
		}
	}
	return out
}

// onE2EEFatal reports a fatal account signal of the encrypted side once, stops that client for good and blocks sending.
func (s *session) onE2EEFatal(reason string) {
	s.mu.Lock()
	first := s.blocked == ""
	if first {
		s.blocked = reason
	}
	s.mu.Unlock()
	go s.guard("e2ee stop", s.stopE2EE)
	if first {
		s.log.Warn().Str("reason", reason).Msg("e2ee fatal signal")
		s.emitConn(api.ConnFatal, reason, api.ActionNone)
	}
}

func (s *session) onCATError(evt *events.CATRefreshError) {
	if errors.Is(evt.Error, mtypes.ErrPleaseReloadPage) {
		s.log.Warn().Msg("e2ee token needs a page reload")
		s.requestRecycle(false)
		return
	}
	s.log.Warn().Msg("e2ee token refresh failed")
	s.emit(api.EvError, api.ErrorEvent{Code: codeE2EECAT, Message: "refreshing the encrypted chat token failed"})
	s.requestRecycle(false)
}

// onConnectFailure follows the connector: 415 re-registers, 418 resets the device and reconnects, once per process.
func (s *session) onConnectFailure(cyc *cycle, evt *events.ConnectFailure) {
	switch evt.Reason {
	case events.ConnectFailureNotFound, events.ConnectFailureClientUnknown:
		go s.guard("e2ee recover", func() { s.recoverE2EE(cyc, evt.Reason) })
	case events.ConnectFailureGeneric:
		s.requestRecycle(false)
	default:
		s.log.Warn().Int("reason", int(evt.Reason)).Msg("e2ee connect failure")
	}
}

func (s *session) recoverE2EE(cyc *cycle, reason events.ConnectFailureReason) {
	if s.blockedReason() != "" {
		return
	}
	s.mu.Lock()
	first := s.e2eeRecoveries == 0
	s.e2eeRecoveries++
	s.mu.Unlock()
	wa := s.e2eeClient()
	s.stopE2EE()
	if !first || cyc.ctx.Err() != nil {
		s.emit(api.EvError, api.ErrorEvent{Code: codeE2EEUnavailable, Message: "encrypted chats are unavailable"})
		return
	}
	if wa != nil && wa.Store != nil {
		ctx, cancel := context.WithTimeout(context.Background(), e2eeOpTimeout)
		err := wa.Store.Delete(ctx)
		cancel()
		if err != nil {
			s.log.Warn().Msg("deleting the e2ee device failed")
		}
	}
	s.setE2EERegistered(false)
	if reason == events.ConnectFailureClientUnknown {
		s.requestRecycle(true)
		return
	}
	s.startE2EE(cyc)
}

func (s *session) onFBMessage(evt *events.FBMessage) bool {
	st := s.e2eeStore()
	if st == nil {
		return false
	}
	res := normalize.FBMessage(evt, normalize.E2EEContext{Me: s.me.Load(), Name: s.names.get})
	ctx, cancel := context.WithTimeout(context.Background(), e2eeOpTimeout)
	defer cancel()
	switch {
	case res.Message != nil:
		return s.onE2EEMessage(ctx, st, evt, res)
	case res.Reaction != nil:
		if _, _, err := st.ApplyReaction(ctx, *res.Reaction); err != nil {
			s.log.Warn().Msg("applying an e2ee reaction failed")
			return false
		}
		r := *res.Reaction
		if r.TS == 0 {
			r.TS = time.Now().Unix()
		}
		s.emit(api.EvReaction, r)
	case res.Edit != nil:
		return s.onE2EEEdit(ctx, st, *res.Edit)
	case res.Removal != nil:
		return s.onE2EERemoval(ctx, st, *res.Removal)
	}
	return true
}

// ignoredChange logs a refused edit or unsend with ids only.
func (s *session) ignoredChange(kind, threadID, messageID string) {
	s.log.Info().Str("op", kind).Str("thread", threadID).Str("id", messageID).Msg("ignored a change from a different sender")
}

// onE2EEEdit applies an edit only when its authenticated sender wrote the target message.
func (s *session) onE2EEEdit(ctx context.Context, st *e2ee.Store, e api.Edit) bool {
	if e.SenderID == "" || e.SenderID != e.TargetSenderID {
		s.ignoredChange("edit", e.ThreadID, e.MessageID)
		return true
	}
	m, found, err := st.ApplyEdit(ctx, e)
	switch {
	case errors.Is(err, e2ee.ErrDenied):
		s.ignoredChange("edit", e.ThreadID, e.MessageID)
	case err != nil:
		s.log.Warn().Msg("applying an e2ee edit failed")
		return false
	case found:
		s.emitMessage(m)
	default:
		text := e.Text
		s.emitMessage(api.Message{
			ID: e.MessageID, ThreadID: e.ThreadID, TS: e.TS, Text: &text, Attachments: []api.Attachment{},
			Reactions: []api.Reaction{}, Edited: true, Encrypted: true, Kind: api.MsgText,
		})
	}
	return true
}

// onE2EERemoval applies an unsend only for the sender of the target message or a known admin of the group.
func (s *session) onE2EERemoval(ctx context.Context, st *e2ee.Store, r api.Removal) bool {
	if r.SenderID == "" {
		s.ignoredChange("unsend", r.ThreadID, r.MessageID)
		return true
	}
	if r.SenderID != r.TargetSenderID {
		sender, _ := strconv.ParseInt(r.SenderID, 10, 64)
		if !s.reg.isAdmin(r.ThreadID, sender) {
			s.ignoredChange("unsend", r.ThreadID, r.MessageID)
			return true
		}
		r.ByAdmin = true
	}
	m, found, err := st.ApplyRemoval(ctx, r)
	switch {
	case errors.Is(err, e2ee.ErrDenied):
		s.ignoredChange("unsend", r.ThreadID, r.MessageID)
	case err != nil:
		s.log.Warn().Msg("applying an e2ee removal failed")
		return false
	case found && r.Unsent:
		s.emitMessage(m)
	case !found && r.Unsent:
		s.emitMessage(api.Message{
			ID: r.MessageID, ThreadID: r.ThreadID, Attachments: []api.Attachment{}, Reactions: []api.Reaction{},
			Unsent: true, Encrypted: true, Kind: api.MsgUnsupported,
		})
	}
	return true
}

func (s *session) onE2EEMessage(ctx context.Context, st *e2ee.Store, evt *events.FBMessage, res normalize.FBResult) bool {
	m := *res.Message
	if stored, existed, err := st.Get(ctx, m.ID); err == nil && existed {
		s.emitMessage(stored)
		return true
	}
	if err := st.Put(ctx, m, res.TSMS, res.Media); err != nil {
		// Put reports a failed prune after the commit too; a stored row means the message is safe.
		if _, existed, gerr := st.Get(ctx, m.ID); gerr != nil || !existed {
			s.log.Warn().Msg("storing an e2ee message failed")
			return false
		}
		s.log.Warn().Msg("pruning the e2ee store failed")
	}
	sender, _ := strconv.ParseInt(m.Sender.ID, 10, 64)
	id, created, changed := s.reg.noteE2EE(evt.Info.Chat, sender, res.TSMS, previewOf(m))
	if changed {
		s.emitThreads([]string{id})
	}
	if created && evt.Info.Chat.Server == types.GroupServer {
		go s.guard("e2ee group", func() { s.refreshGroup(evt.Info.Chat) })
	}
	s.emitMessage(m)
	return true
}

// refreshGroup names an encrypted group thread and lists its members; failures are ignored.
func (s *session) refreshGroup(chat types.JID) {
	wa := s.e2eeClient()
	if wa == nil {
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), e2eeOpTimeout)
	defer cancel()
	info, err := wa.GetGroupInfo(ctx, chat)
	if err != nil || info == nil {
		return
	}
	members := make([]int64, 0, len(info.Participants))
	var admins []int64
	for _, p := range info.Participants {
		member := int64(p.JID.UserInt())
		members = append(members, member)
		if p.IsAdmin || p.IsSuperAdmin {
			admins = append(admins, member)
		}
	}
	id := normalize.ThreadIDFromJID(chat)
	if s.reg.setGroupInfo(id, info.Name, members, admins) {
		s.emitThreads([]string{id})
	}
}

func (s *session) onReceipt(evt *events.Receipt) {
	kind := ""
	switch evt.Type {
	case types.ReceiptTypeRead, types.ReceiptTypeReadSelf:
		kind = "read"
	case types.ReceiptTypeDelivered:
		kind = "delivered"
	default:
		return
	}
	s.emit(api.EvReceipt, api.ReceiptEvent{
		ThreadID: normalize.ThreadIDFromJID(evt.Chat),
		Kind:     kind,
		SenderID: strconv.FormatUint(evt.Sender.UserInt(), 10),
		UpToTS:   evt.Timestamp.Unix(),
	})
}
