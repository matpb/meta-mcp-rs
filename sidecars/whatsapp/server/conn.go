package server

import (
	"context"
	"errors"
	"fmt"
	"strconv"
	"strings"
	"time"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/ipc"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/store"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/wa"
)

// Store meta keys.
const (
	metaPairedAt    = "paired_at"
	metaFullHistory = "full_history"
	metaOwnerJID    = "owner_jid"
)

func parseInt(s string) int64 {
	n, _ := strconv.ParseInt(s, 10, 64)
	return n
}

// connect opens the WhatsApp connection of the linked device.
func (s *server) connect(ctx context.Context, req ipc.Request) ipc.Reply {
	cl := s.cur()
	if !cl.HasDevice() {
		return fail(req.ID, api.CodeNotLoggedIn, "no WhatsApp account is linked")
	}
	if r := s.blockedReason(); r != "" {
		return failErr(req.ID, notConnectedBlocked(r))
	}
	if cl.IsConnected() {
		return ok(req.ID, struct{}{})
	}
	s.factory.RefreshVersion(ctx)
	s.emitLog(api.EvConnection, api.ConnectionEvent{State: api.ConnConnecting})
	s.setWantConn(true)
	if err := cl.Connect(); err != nil {
		s.setWantConn(false)
		s.emitLog(api.EvConnection, api.ConnectionEvent{State: api.ConnDisconnected, Reason: "connect_error", Action: api.ActionWait})
		return fail(req.ID, api.CodeIO, "could not connect to WhatsApp")
	}
	return ok(req.ID, struct{}{})
}

// disconnect closes the connection on request.
func (s *server) disconnect(_ context.Context, req ipc.Request) ipc.Reply {
	s.setWantConn(false)
	s.cur().Disconnect()
	s.emitLog(api.EvConnection, api.ConnectionEvent{State: api.ConnDisconnected, Reason: "requested", Action: api.ActionNone})
	return ok(req.ID, struct{}{})
}

// blockedReason returns the fatal reason that refuses sends, or "".
func (s *server) blockedReason() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.blocked
}

func (s *server) setBlocked(reason string) {
	s.mu.Lock()
	s.blocked = reason
	s.mu.Unlock()
}

func blockedError(reason string) *api.Error {
	return api.Errorf(api.CodeSendingBlocked, "sending is blocked (%s); restart the sidecar after the cause is resolved", reason)
}

// notConnectedBlocked is the connect refusal while a fatal signal holds the account.
func notConnectedBlocked(reason string) *api.Error {
	return api.Errorf(api.CodeNotConnected, "not connected; the account is blocked (%s); restart the sidecar after the cause is resolved", reason)
}

// fatal blocks sends with reason and tells the daemon. Except for the reach-out time-lock, which leaves the
// connection up, it also ends the wish to be connected, so the socket closing is not reported as a retryable drop.
func (s *server) fatal(reason, action string) error {
	s.setBlocked(reason)
	if reason != api.FatalReachoutTimelock {
		s.setWantConn(false)
	}
	s.log.Info().Str("reason", reason).Msg("fatal connection state")
	return s.emit(api.EvConnection, api.ConnectionEvent{State: api.ConnFatal, Reason: reason, Action: action})
}

// sendGate applies the shared send checks and resolves the target chat.
func (s *server) sendGate(ctx context.Context, threadID string) (wa.Client, store.Chat, error) {
	if r := s.blockedReason(); r != "" {
		return nil, store.Chat{}, blockedError(r)
	}
	cl := s.cur()
	if !cl.HasDevice() {
		return nil, store.Chat{}, api.Errorf(api.CodeNotLoggedIn, "no WhatsApp account is linked")
	}
	if !cl.IsConnected() {
		return nil, store.Chat{}, api.Errorf(api.CodeNotConnected, "not connected to WhatsApp")
	}
	chat, err := s.chatFor(ctx, threadID)
	if err != nil {
		return nil, store.Chat{}, err
	}
	return cl, chat, nil
}

// chatFor finds a stored chat by either of its ids.
func (s *server) chatFor(ctx context.Context, threadID string) (store.Chat, error) {
	chat, found, err := s.store.GetChat(ctx, threadID)
	if err != nil {
		return store.Chat{}, api.Errorf(api.CodeIO, "message database error")
	}
	if !found {
		if owner, ok, err := s.store.GetMeta(ctx, metaOwnerJID); err == nil && ok && owner != "" && owner == threadID {
			// The owner's own chat (message yourself) is always a safe target: create it on first use.
			chat, _, err = s.store.EnsureChat(ctx, owner, "", api.KindDM)
			if err != nil {
				return store.Chat{}, api.Errorf(api.CodeIO, "message database error")
			}
			return chat, nil
		}
		return store.Chat{}, api.Errorf(api.CodeThreadNotFound, "unknown thread; the owner must open this chat on the phone first")
	}
	return chat, nil
}

// handleEvent runs on whatsmeow's goroutine: it commits to the store and stdout, never waits on the network.
// A panic is recovered and answered with false, so the event stays unacknowledged.
func (s *server) handleEvent(cl wa.Client, evt any) (handled bool) {
	defer func() {
		if r := recover(); r != nil {
			s.recovered("event", fmt.Sprintf("%T", evt), r)
			handled = false
		}
	}()
	if s.closed.Load() {
		return false
	}
	if s.cur() != cl {
		return true
	}
	if err := s.dispatch(s.ctx, cl, evt); err != nil {
		s.log.Error().Str("event", fmt.Sprintf("%T", evt)).Msg("event handling failed")
		return false
	}
	return true
}

func (s *server) dispatch(ctx context.Context, cl wa.Client, evt any) error {
	switch e := evt.(type) {
	case *events.Message:
		return s.onMessage(ctx, cl, e)
	case *events.HistorySync:
		return s.onHistorySync(ctx, cl, e)
	case *events.Receipt:
		return s.onReceipt(ctx, cl, e)
	case *events.MediaRetry:
		s.media.deliver(e)
		return nil
	case *events.UndecryptableMessage:
		s.log.Info().Msg("undecryptable message")
		return nil
	case *events.Pin, *events.Archive, *events.Mute, *events.MarkChatAsRead, *events.DeleteChat,
		*events.Contact, *events.PushName, *events.JoinedGroup, *events.GroupInfo:
		return s.onAppState(ctx, cl, evt)

	case *events.PairSuccess:
		return s.onPairSuccess(ctx, e)
	case *events.Connected:
		return s.onConnected(cl)
	case *events.Disconnected:
		if !s.wantsConn() || s.loginActive() {
			return nil
		}
		return s.emit(api.EvConnection, api.ConnectionEvent{State: api.ConnDisconnected, Reason: "network", Action: api.ActionWait})
	case *events.KeepAliveTimeout:
		return s.emit(api.EvConnection, api.ConnectionEvent{State: api.ConnDisconnected, Reason: "keepalive_timeout", Action: api.ActionWait})
	case *events.KeepAliveRestored:
		return s.emit(api.EvConnection, api.ConnectionEvent{State: api.ConnConnected})
	case *events.ConnectFailure:
		if e.Reason == events.ConnectFailureBadUserAgent {
			return s.fatal(api.FatalStreamReplaced, api.ActionNone)
		}
		return s.emit(api.EvConnection, api.ConnectionEvent{
			State: api.ConnDisconnected, Reason: "connect_failure_" + strconv.Itoa(int(e.Reason)), Action: api.ActionRetry,
		})
	case *events.TemporaryBan:
		return s.fatal(api.FatalTemporaryBan, api.ActionWait)
	case *events.NotifyAccountReachoutTimelock:
		if !e.IsActive {
			return nil
		}
		return s.fatal(api.FatalReachoutTimelock, api.ActionWait)
	case *events.StreamReplaced:
		return s.fatal(api.FatalStreamReplaced, api.ActionNone)
	case *events.ClientOutdated:
		return s.fatal(api.FatalClientOutdated, api.ActionNone)
	case *events.LoggedOut:
		return s.onLoggedOut(cl, e)
	}
	return nil
}

// onConnected marks the account unavailable so the phone keeps its notifications, then reports the connection.
func (s *server) onConnected(cl wa.Client) error {
	s.bg.Add(1)
	go func() {
		defer s.bg.Done()
		defer s.recoverPanic("presence")
		ctx, cancel := context.WithTimeout(s.ctx, 10*time.Second)
		defer cancel()
		_ = cl.SendPresence(ctx, types.PresenceUnavailable)
	}()
	s.markConnected()
	return s.emit(api.EvConnection, api.ConnectionEvent{State: api.ConnConnected})
}

// onLoggedOut reports a removed or banned device and swaps in a fresh unpaired client in the background.
func (s *server) onLoggedOut(cl wa.Client, e *events.LoggedOut) error {
	reason := api.FatalDeviceRemoved
	if e.Reason == events.ConnectFailureUnknownLogout {
		reason = api.FatalBanned
	}
	if err := s.fatal(reason, api.ActionRelogin); err != nil {
		return err
	}
	if err := s.emit(api.EvLoggedOut, api.LoggedOutEvent{Reason: reason}); err != nil {
		return err
	}
	if err := s.emit(api.EvStateUpdated, api.StateUpdatedEvent{}); err != nil {
		return err
	}
	s.bg.Add(1)
	go func() {
		defer s.bg.Done()
		defer s.recoverPanic("logged_out")
		unlinked := s.awaitUnlinked(cl)
		if !s.isCurrent(cl) || s.closed.Load() {
			return
		}
		if !unlinked {
			s.log.Info().Msg("the device record was not removed in time, the session database wipe is skipped")
		} else if err := s.factory.Scrub(s.ctx); err != nil {
			s.log.Error().Msg("could not wipe the session database after the unlink")
		}
		if !s.isCurrent(cl) {
			return
		}
		if err := s.freshClient(s.ctx); err != nil {
			s.log.Error().Msg("could not create a fresh client after logout")
		}
	}()
	return nil
}

// isCurrent reports whether cl is still the client the server uses.
func (s *server) isCurrent(cl wa.Client) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.client == cl
}

// awaitUnlinked waits for whatsmeow to finish deleting the device record it announced with LoggedOut.
func (s *server) awaitUnlinked(cl wa.Client) bool {
	deadline := time.Now().Add(s.tm.unlink)
	for cl.HasDevice() {
		if !time.Now().Before(deadline) {
			return false
		}
		select {
		case <-time.After(20 * time.Millisecond):
		case <-s.ctx.Done():
			return false
		}
	}
	return true
}

// sendError maps a whatsmeow send failure to a protocol error and raises the 463 fatal state.
func (s *server) sendError(err error, id string) *api.Error {
	var disc *whatsmeow.DisconnectedError
	switch {
	case errors.As(err, &disc), errors.Is(err, whatsmeow.ErrNotConnected) && id != "":
		return unknownOutcome(id)
	case errors.Is(err, whatsmeow.ErrServerReturnedError) && strings.HasSuffix(err.Error(), " 463"):
		_ = s.fatal(api.FatalReachoutTimelock, api.ActionWait)
		return blockedError(api.FatalReachoutTimelock)
	case errors.Is(err, whatsmeow.ErrMessageTimedOut), errors.Is(err, context.DeadlineExceeded), errors.Is(err, context.Canceled):
		return unknownOutcome(id)
	case errors.Is(err, whatsmeow.ErrNotConnected):
		return api.Errorf(api.CodeNotConnected, "not connected to WhatsApp")
	case errors.Is(err, whatsmeow.ErrNotLoggedIn):
		return api.Errorf(api.CodeNotLoggedIn, "no WhatsApp account is linked")
	}
	return api.Errorf(api.CodeSendFailed, "WhatsApp refused or failed the request")
}

// unknownOutcome is the send_unknown error for a request that may have reached WhatsApp.
func unknownOutcome(id string) *api.Error {
	if id == "" {
		return api.Errorf(api.CodeSendUnknown, "the outcome of the request is unknown")
	}
	return api.Errorf(api.CodeSendUnknown, "the outcome of the send is unknown; message id %s may have been delivered", id)
}

// onPairSuccess switches the stored owner before Connected or any history can arrive.
func (s *server) onPairSuccess(ctx context.Context, e *events.PairSuccess) error {
	s.ingestMu.Lock()
	defer s.ingestMu.Unlock()
	self := e.ID.ToNonAD().String()
	owner, found, err := s.store.GetMeta(ctx, metaOwnerJID)
	if err != nil {
		return err
	}
	if found && owner != "" && owner != self {
		if err := s.store.Reset(ctx); err != nil {
			return err
		}
	}
	return s.store.SetMeta(ctx, metaOwnerJID, self)
}
