package engine

import (
	"context"
	"errors"
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

// ensureStore opens the E2EE database on first use.
func (s *session) ensureStore(ctx context.Context) (*e2ee.Store, error) {
	s.e2mu.Lock()
	defer s.e2mu.Unlock()
	if s.closed.Load() {
		return nil, api.Errorf(api.CodeNotLoggedIn, "session closed")
	}
	if s.st != nil {
		return s.st, nil
	}
	st, err := e2ee.Open(ctx, s.dataDir, s.log.With().Str("component", "e2ee").Logger())
	if err != nil {
		return nil, api.AsError(err)
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
func (s *session) startE2EE(cyc *cycle) {
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
	return nil
}

// e2eeHandler returns whatsmeow's handler; false (no ack) asks for redelivery, also after a panic.
func (s *session) e2eeHandler(cyc *cycle) func(evt any) bool {
	return func(raw any) (ok bool) {
		defer func() {
			if r := recover(); r != nil {
				s.log.Error().Msg("panic in e2ee handler")
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
			s.log.Warn().Msg("e2ee logged out")
			s.emit(api.EvError, api.ErrorEvent{Code: codeE2EELoggedOut, Message: "the encrypted chat session was logged out"})
			go s.stopE2EE()
		case *events.CATRefreshError:
			s.onCATError(evt)
		}
		return true
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
		go s.recoverE2EE(cyc, evt.Reason)
	case events.ConnectFailureGeneric:
		s.requestRecycle(false)
	default:
		s.log.Warn().Int("reason", int(evt.Reason)).Msg("e2ee connect failure")
	}
}

func (s *session) recoverE2EE(cyc *cycle, reason events.ConnectFailureReason) {
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
		e := *res.Edit
		m, found, err := st.ApplyEdit(ctx, e)
		if err != nil {
			s.log.Warn().Msg("applying an e2ee edit failed")
			return false
		}
		if found {
			s.emitMessage(m)
			break
		}
		text := e.Text
		s.emitMessage(api.Message{
			ID: e.MessageID, ThreadID: e.ThreadID, TS: e.TS, Text: &text, Attachments: []api.Attachment{},
			Reactions: []api.Reaction{}, Edited: true, Encrypted: true, Kind: api.MsgText,
		})
	case res.Removal != nil:
		r := *res.Removal
		m, found, err := st.ApplyRemoval(ctx, r)
		if err != nil {
			s.log.Warn().Msg("applying an e2ee removal failed")
			return false
		}
		switch {
		case found && r.Unsent:
			s.emitMessage(m)
		case !found && r.Unsent:
			s.emitMessage(api.Message{
				ID: r.MessageID, ThreadID: r.ThreadID, Attachments: []api.Attachment{}, Reactions: []api.Reaction{},
				Unsent: true, Encrypted: true, Kind: api.MsgUnsupported,
			})
		}
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
		go s.refreshGroup(evt.Info.Chat)
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
	for _, p := range info.Participants {
		members = append(members, int64(p.JID.UserInt()))
	}
	id := normalize.ThreadIDFromJID(chat)
	if s.reg.setGroupInfo(id, info.Name, members) {
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
