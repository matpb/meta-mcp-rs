package server

import (
	"context"
	"encoding/json"
	"errors"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/types"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/ipc"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/wa"
)

const (
	defaultDeviceName = "meta-mcp"
	awaitPasskey      = "passkey_response"
	awaitConfirm      = "confirm"
)

const passkeyInstructions = "Open https://web.whatsapp.com in a Chrome-based browser, open the developer console and run " +
	"console.log(JSON.stringify((await navigator.credentials.get({publicKey: PublicKeyCredential.parseRequestOptionsFromJSON(<public_key>)})).toJSON())) " +
	"with <public_key> replaced by the public_key object of this event. Pass the printed JSON as passkey_response to login_finish."

type pairResult struct {
	code string
	err  error
}

// login is one pairing attempt; a new login_start replaces it.
type login struct {
	method      string
	phone       string
	fullHistory bool
	client      wa.Client
	cancel      context.CancelFunc
	done        chan struct{}   // closed when the reader goroutine exits
	connected   chan struct{}   // closed by the Connected event
	connOnce    sync.Once       // guards connected
	pairRes     chan pairResult // first pair code or its error
	gotCode     atomic.Bool
	endOnce     sync.Once

	mu       sync.Mutex
	awaiting string
}

func (l *login) setAwaiting(v string) {
	l.mu.Lock()
	l.awaiting = v
	l.mu.Unlock()
}

func (l *login) takeAwaiting(want string) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.awaiting != want {
		return false
	}
	l.awaiting = ""
	return true
}

// markConnected sets the Connected latch of the active login.
func (s *server) markConnected() {
	s.mu.Lock()
	l := s.login
	s.mu.Unlock()
	if l != nil {
		l.connOnce.Do(func() { close(l.connected) })
	}
}

func (s *server) loginActive() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.login != nil
}

type loginStartParams struct {
	Method      string `json:"method"`
	Phone       string `json:"phone"`
	FullHistory bool   `json:"full_history"`
	DeviceName  string `json:"device_name"`
}

// normalizePhone strips formatting and checks the 7..15 digit international form.
func normalizePhone(p string) (string, bool) {
	p = strings.NewReplacer(" ", "", "-", "", "(", "", ")", "").Replace(strings.TrimPrefix(strings.TrimSpace(p), "+"))
	if len(p) < 7 || len(p) > 15 || p[0] == '0' {
		return "", false
	}
	for _, c := range p {
		if c < '0' || c > '9' {
			return "", false
		}
	}
	return p, true
}

// loginStart begins QR or pair-code pairing on a fresh unpaired client.
func (s *server) loginStart(ctx context.Context, req ipc.Request) ipc.Reply {
	var p loginStartParams
	if e := decode(req, &p); e != nil {
		return failErr(req.ID, e)
	}
	s.loginMu.Lock()
	defer s.loginMu.Unlock()
	if s.cur().HasDevice() {
		return fail(req.ID, api.CodeAlreadyLoggedIn, "a WhatsApp account is already linked; log out first")
	}
	phone := ""
	switch p.Method {
	case "qr":
	case "pair_code":
		var valid bool
		if phone, valid = normalizePhone(p.Phone); !valid {
			return fail(req.ID, api.CodeBadParams, "phone must be 7 to 15 digits in international form without a leading 0")
		}
	default:
		return fail(req.ID, api.CodeBadParams, "method must be qr or pair_code")
	}

	s.endActiveLogin()
	s.mu.Lock()
	old := s.client
	s.wantConn = false
	s.mu.Unlock()
	old.Disconnect()
	name := p.DeviceName
	if name == "" {
		name = defaultDeviceName
	}
	cl, err := s.factory.NewClient(ctx, &wa.LoginOptions{DeviceName: name, FullHistory: p.FullHistory})
	if err != nil {
		return fail(req.ID, api.CodeIO, "could not prepare a new WhatsApp client")
	}
	s.setClient(cl)
	s.factory.RefreshVersion(ctx)
	if p.Method == "pair_code" {
		cl.SetPrePairCallback(func(jid types.JID, _, _ string) bool { return jid.User == phone })
	}

	lctx, lcancel := context.WithCancel(s.ctx)
	l := &login{
		method: p.Method, phone: phone, fullHistory: p.FullHistory, client: cl, cancel: lcancel,
		done: make(chan struct{}), connected: make(chan struct{}), pairRes: make(chan pairResult, 1),
	}
	s.mu.Lock()
	s.login = l
	s.mu.Unlock()

	ch, err := cl.GetQRChannel(lctx)
	if err != nil {
		close(l.done)
		s.endLogin(l)
		return fail(req.ID, api.CodeLoginFailed, "could not start the pairing channel")
	}
	s.setWantConn(true)
	if err := cl.Connect(); err != nil {
		close(l.done)
		s.endLogin(l)
		return fail(req.ID, api.CodeLoginFailed, "could not connect to WhatsApp")
	}
	s.bg.Add(1)
	go s.runLogin(lctx, l, ch)

	if p.Method == "qr" {
		return ok(req.ID, struct{}{})
	}
	timer := time.NewTimer(s.tm.pairCode)
	defer timer.Stop()
	select {
	case res := <-l.pairRes:
		if res.err != nil {
			return failErr(req.ID, pairError(res.err))
		}
		return ok(req.ID, map[string]string{"code": formatPairCode(res.code)})
	case <-timer.C:
		s.endLogin(l)
		return fail(req.ID, api.CodeLoginFailed, "WhatsApp did not offer a pairing code in time")
	case <-lctx.Done():
		return fail(req.ID, api.CodeLoginFailed, "the login was cancelled")
	case <-ctx.Done():
		s.endLogin(l)
		return fail(req.ID, api.CodeLoginFailed, "the login was cancelled")
	}
}

func pairError(err error) *api.Error {
	switch {
	case errors.Is(err, whatsmeow.ErrIQRateOverLimit):
		return api.Errorf(api.CodeRateLimited, "WhatsApp is rate limiting pairing; try again later")
	case errors.Is(err, whatsmeow.ErrPhoneNumberTooShort), errors.Is(err, whatsmeow.ErrPhoneNumberIsNotInternational):
		return api.Errorf(api.CodeBadParams, "phone must be in international form")
	}
	return api.Errorf(api.CodeLoginFailed, "WhatsApp refused the pairing code request")
}

func formatPairCode(c string) string {
	if len(c) == 8 && !strings.Contains(c, "-") {
		return c[:4] + "-" + c[4:]
	}
	return c
}

// runLogin reads the QR channel until a terminal item, cancellation or channel close.
func (s *server) runLogin(ctx context.Context, l *login, ch <-chan whatsmeow.QRChannelItem) {
	defer s.bg.Done()
	defer close(l.done)
	for {
		select {
		case <-ctx.Done():
			return
		case item, open := <-ch:
			if !open {
				return
			}
			if s.onQRItem(ctx, l, item) {
				return
			}
		}
	}
}

// onQRItem handles one channel item and reports whether the login is over.
func (s *server) onQRItem(ctx context.Context, l *login, item whatsmeow.QRChannelItem) bool {
	switch item.Event {
	case whatsmeow.QRChannelEventCode:
		if l.method == "qr" {
			s.emitLog(api.EvQR, api.QREvent{Code: item.Code, ExpiresInS: int(item.Timeout / time.Second)})
			return false
		}
		if l.gotCode.CompareAndSwap(false, true) {
			code, err := l.client.PairPhone(ctx, l.phone)
			l.pairRes <- pairResult{code: code, err: err}
			if err != nil {
				s.endLogin(l)
				return true
			}
		}
	case whatsmeow.QRChannelEventPasskeyRequest:
		if item.PasskeyRequest == nil {
			return false
		}
		l.setAwaiting(awaitPasskey)
		s.emitLog(api.EvPasskeyReq, api.PasskeyRequestEvent{PublicKey: item.PasskeyRequest.PublicKey, Instructions: passkeyInstructions})
	case whatsmeow.QRChannelEventPasskeyResponse:
		if item.PasskeyConfirmation == nil {
			return false
		}
		code := item.PasskeyConfirmation.Code
		if item.PasskeyConfirmation.SkipHandoffUX {
			code = ""
		}
		l.setAwaiting(awaitConfirm)
		s.emitLog(api.EvPasskeyConf, api.PasskeyConfirmationEvent{Code: code})
	case whatsmeow.QRChannelSuccess.Event:
		s.finishPairing(ctx, l)
		return true
	case whatsmeow.QRChannelTimeout.Event:
		s.emitLog(api.EvConnection, api.ConnectionEvent{State: api.ConnDisconnected, Reason: "qr_timeout", Action: api.ActionRetry})
		s.endLogin(l)
		return true
	case whatsmeow.QRChannelClientOutdated.Event:
		_ = s.fatal(api.FatalClientOutdated, api.ActionNone)
		s.endLogin(l)
		return true
	default:
		msg := "pairing failed (" + item.Event + ")"
		s.emitLog(api.EvError, api.ErrorEvent{Code: api.CodeLoginFailed, Message: msg})
		s.emitLog(api.EvConnection, api.ConnectionEvent{State: api.ConnDisconnected, Reason: "pair_error", Action: api.ActionRetry})
		s.endLogin(l)
		return true
	}
	return false
}

// finishPairing waits for Connected and the prekey upload, then stores the owner and reports logged_in.
func (s *server) finishPairing(ctx context.Context, l *login) {
	defer s.clearLogin(l)
	sctx, cancel := context.WithTimeout(ctx, s.tm.setup)
	defer cancel()
	deadline, _ := sctx.Deadline()
	select {
	case <-l.connected:
	case <-sctx.Done():
		s.setupFailed(ctx)
		return
	}
	if err := l.client.WaitPreKeysUploaded(sctx, time.Until(deadline)); err != nil {
		s.setupFailed(ctx)
		return
	}
	if err := s.recordOwner(ctx, l); err != nil {
		s.emitLog(api.EvError, api.ErrorEvent{Code: api.CodeLoginFailed, Message: "paired but the account state could not be stored; call connect"})
		return
	}
	acc := accountOf(l.client)
	if acc == nil {
		s.setupFailed(ctx)
		return
	}
	s.emitLog(api.EvStateUpdated, api.StateUpdatedEvent{State: s.stateBlob(ctx, l.client)})
	s.emitLog(api.EvLoggedIn, api.LoggedInEvent{Account: *acc})
}

// setupFailed reports a pairing that succeeded on the phone but did not finish here.
func (s *server) setupFailed(ctx context.Context) {
	if ctx.Err() != nil {
		return
	}
	s.emitLog(api.EvError, api.ErrorEvent{Code: api.CodeLoginFailed, Message: "paired but setup did not finish; call connect"})
}

// recordOwner stores the pairing time and history flag; the owner switch happens on PairSuccess.
func (s *server) recordOwner(ctx context.Context, l *login) error {
	for k, v := range map[string]string{
		metaPairedAt:    strconv.FormatInt(time.Now().Unix(), 10),
		metaFullHistory: strconv.FormatBool(l.fullHistory),
	} {
		if err := s.store.SetMeta(ctx, k, v); err != nil {
			return err
		}
	}
	return nil
}

// clearLogin forgets l when it is still the active login.
func (s *server) clearLogin(l *login) {
	s.mu.Lock()
	if s.login == l {
		s.login = nil
	}
	s.mu.Unlock()
}

// endLogin cancels l, disconnects its client and, when it is still current, installs a fresh idle client.
func (s *server) endLogin(l *login) {
	l.endOnce.Do(func() {
		l.cancel()
		l.client.Disconnect()
		s.mu.Lock()
		current := s.client == l.client
		if s.login == l {
			s.login = nil
		}
		s.mu.Unlock()
		if current && !s.closed.Load() {
			if err := s.freshClient(s.ctx); err != nil {
				s.log.Error().Msg("could not create a fresh client after login")
			}
		}
	})
}

// endActiveLogin cancels the running login, if any, and waits briefly for its reader to stop.
func (s *server) endActiveLogin() {
	s.mu.Lock()
	l := s.login
	s.mu.Unlock()
	if l == nil {
		return
	}
	l.cancel()
	select {
	case <-l.done:
	case <-time.After(2 * time.Second):
	}
	s.endLogin(l)
}

type loginFinishParams struct {
	PasskeyResponse json.RawMessage `json:"passkey_response"`
	Confirm         bool            `json:"confirm"`
}

// loginFinish answers a passkey request or confirmation of the running login.
func (s *server) loginFinish(ctx context.Context, req ipc.Request) ipc.Reply {
	var p loginFinishParams
	if e := decode(req, &p); e != nil {
		return failErr(req.ID, e)
	}
	s.mu.Lock()
	l := s.login
	s.mu.Unlock()
	hasResp := len(p.PasskeyResponse) > 0 && string(p.PasskeyResponse) != "null"
	switch {
	case l == nil || (!hasResp && !p.Confirm):
		return fail(req.ID, api.CodeBadParams, "no login step is waiting for this answer")
	case hasResp:
		var resp types.WebAuthnResponse
		if json.Unmarshal(p.PasskeyResponse, &resp) != nil {
			return fail(req.ID, api.CodeBadParams, "passkey_response is not a valid WebAuthn assertion")
		}
		if !l.takeAwaiting(awaitPasskey) {
			return fail(req.ID, api.CodeBadParams, "no passkey request is pending")
		}
		if err := l.client.SendPasskeyResponse(ctx, &resp); err != nil {
			l.setAwaiting(awaitPasskey)
			return fail(req.ID, api.CodeLoginFailed, "WhatsApp rejected the passkey response")
		}
	default:
		if !l.takeAwaiting(awaitConfirm) {
			return fail(req.ID, api.CodeBadParams, "no passkey confirmation is pending")
		}
		if err := l.client.SendPasskeyConfirmation(ctx); err != nil {
			l.setAwaiting(awaitConfirm)
			return fail(req.ID, api.CodeLoginFailed, "WhatsApp rejected the passkey confirmation")
		}
	}
	return ok(req.ID, struct{}{})
}

// loginCancel stops a running login; once the device is paired it lets setup finish.
func (s *server) loginCancel(_ context.Context, req ipc.Request) ipc.Reply {
	s.mu.Lock()
	l := s.login
	s.mu.Unlock()
	if l != nil && !l.client.HasDevice() {
		l.cancel()
		s.endLogin(l)
	}
	return ok(req.ID, struct{}{})
}

// logout unlinks the device, wipes the message store and installs a fresh unpaired client.
func (s *server) logout(ctx context.Context, req ipc.Request) ipc.Reply {
	s.loginMu.Lock()
	defer s.loginMu.Unlock()
	s.endActiveLogin()
	cl := s.cur()
	s.setWantConn(false)
	done := false
	if cl.HasDevice() && cl.IsConnected() {
		done = cl.Logout(ctx) == nil
	}
	if !done {
		cl.Disconnect()
		if cl.HasDevice() {
			if err := cl.DeleteDevice(ctx); err != nil {
				return fail(req.ID, api.CodeIO, "could not delete the linked device")
			}
		}
	}
	if err := s.store.Reset(ctx); err != nil {
		return fail(req.ID, api.CodeIO, "could not reset the message database")
	}
	s.emitLog(api.EvStateUpdated, api.StateUpdatedEvent{})
	if err := s.freshClient(ctx); err != nil {
		return fail(req.ID, api.CodeIO, "could not prepare a new WhatsApp client")
	}
	return ok(req.ID, struct{}{})
}
