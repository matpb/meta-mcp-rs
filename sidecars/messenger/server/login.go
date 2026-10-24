package server

import (
	"context"
	"encoding/json"
	"errors"
	"regexp"
	"strings"
	"time"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
	"github.com/matpb/meta-mcp-rs/sidecars/messenger/ipc"
)

const (
	validateTimeout = 90 * time.Second
	maxCookies      = 32
	maxCookieValue  = 4096
	maxUserAgent    = 512
)

var cookieName = regexp.MustCompile(`^[A-Za-z0-9_.-]{1,64}$`)

const loginInstructions = "Log in to www.facebook.com in Chrome on Linux (the sidecar presents a Linux Chrome user agent), " +
	"open DevTools > Application > Cookies > https://www.facebook.com, and copy the values of c_user, xs, datr, sb and fr " +
	"as a JSON object. Do not log out of that browser session afterwards, because logging out invalidates the cookies; " +
	"close the window instead."

func (s *server) loginStart(req ipc.Request) ipc.Reply {
	var p struct {
		Method string `json:"method"`
	}
	if !decodeParams(req.Params, &p) || p.Method != "cookies" {
		return fail(req.ID, api.CodeBadParams, "method must be cookies")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.sess != nil {
		return fail(req.ID, api.CodeAlreadyLoggedIn, "already logged in")
	}
	if s.login == loginFinishing {
		return fail(req.ID, api.CodeBadParams, "login in progress")
	}
	s.login = loginAwaiting
	return ok(req.ID, map[string]any{
		"needs":        []string{"cookies"},
		"cookie_names": []string{"c_user", "xs", "datr", "sb", "fr"},
		"instructions": loginInstructions,
	})
}

func (s *server) loginCancelOp(req ipc.Request) ipc.Reply {
	s.mu.Lock()
	s.abortLoginLocked()
	s.mu.Unlock()
	return ok(req.ID, struct{}{})
}

// abortLoginLocked cancels an in-flight finish and resets the state; s.mu must be held.
func (s *server) abortLoginLocked() {
	if s.loginCancel != nil {
		s.loginCancel()
		s.loginCancel = nil
	}
	s.loginGen++
	if s.sess == nil {
		s.login = loginNone
	}
}

type finishParams struct {
	Cookies   map[string]string `json:"cookies"`
	UserAgent string            `json:"user_agent"`
}

// loginFinish validates inline; the returned work func, when non-nil, runs asynchronously.
func (s *server) loginFinish(req ipc.Request) (ipc.Reply, func() ipc.Reply) {
	s.mu.Lock()
	state := s.login
	s.mu.Unlock()
	switch state {
	case loginFinishing:
		return fail(req.ID, api.CodeBadParams, "login in progress"), nil
	case loginNone:
		return fail(req.ID, api.CodeBadParams, "call login_start first"), nil
	}
	var p finishParams
	if !decodeParams(req.Params, &p) {
		return fail(req.ID, api.CodeBadParams, "cookies must be an object of strings"), nil
	}
	if msg := validateCookies(p.Cookies); msg != "" {
		return fail(req.ID, api.CodeBadParams, msg), nil
	}
	if !validUserAgent(p.UserAgent) {
		return fail(req.ID, api.CodeBadParams, "invalid user_agent"), nil
	}

	lctx, cancel := context.WithCancel(s.ctx)
	s.mu.Lock()
	if s.login != loginAwaiting {
		s.mu.Unlock()
		cancel()
		return fail(req.ID, api.CodeBadParams, "call login_start first"), nil
	}
	s.login = loginFinishing
	s.loginGen++
	gen := s.loginGen
	s.loginCancel = cancel
	dataDir := s.dataDir
	s.mu.Unlock()

	state0 := api.State{Cookies: p.Cookies, UserAgent: p.UserAgent, FBID: p.Cookies["c_user"]}
	return ipc.Reply{}, func() ipc.Reply {
		defer cancel()
		return s.finishWork(req.ID, lctx, gen, state0, dataDir)
	}
}

func (s *server) finishWork(id uint64, ctx context.Context, gen uint64, st api.State, dataDir string) (rep ipc.Reply) {
	var sess api.Session
	defer func() {
		if recover() != nil {
			s.log.Error().Str("op", "login_finish").Msg("handler panicked")
			rep = s.finishFailed(id, gen, sess, api.Errorf(api.CodeInternal, "internal error"))
		}
	}()
	sink, ref := s.newSink()
	sess, err := s.f.Open(st, dataDir, sink)
	if err != nil {
		return s.finishFailed(id, gen, nil, err)
	}
	vctx, cancel := context.WithTimeout(ctx, validateTimeout)
	defer cancel()
	acct, err := sess.Validate(vctx)
	if err != nil {
		return s.finishFailed(id, gen, sess, err)
	}
	if s.hooks.afterValidate != nil {
		s.hooks.afterValidate()
	}
	switch err := s.install(sess, ref, &gen); {
	case errors.Is(err, errLoginCancelled):
		sess.Close()
		return fail(id, api.CodeLoginFailed, "login cancelled")
	case err != nil:
		return fail(id, api.CodeLoginFailed, "session ended during login")
	}
	s.emit(api.EvStateUpdated, api.StateUpdatedEvent{State: ptr(sess.State())})
	s.emit(api.EvLoggedIn, api.LoggedInEvent{Account: acct})
	return ok(id, map[string]any{"account": acct})
}

// finishFailed resets the login state and builds the error reply; fail-closed errors also emit logged_out.
func (s *server) finishFailed(id uint64, gen uint64, sess api.Session, err error) ipc.Reply {
	if sess != nil {
		sess.Close()
	}
	s.mu.Lock()
	cancelled := s.loginGen != gen
	if !cancelled {
		s.login = loginAwaiting
		s.loginCancel = nil
	}
	s.mu.Unlock()
	if cancelled {
		return fail(id, api.CodeLoginFailed, "login cancelled")
	}
	if e := api.AsError(err); e.Reason != "" {
		s.emit(api.EvLoggedOut, api.LoggedOutEvent{Reason: e.Reason, URL: e.URL})
	}
	return failErr(id, err)
}

func (s *server) logout(ctx context.Context, req ipc.Request) ipc.Reply {
	s.mu.Lock()
	sess := s.sess
	s.sess = nil
	s.abortLoginLocked()
	s.login = loginNone
	s.mu.Unlock()
	if sess != nil {
		if err := sess.Logout(ctx); err != nil {
			s.log.Warn().Str("code", api.AsError(err).Code).Msg("logout failed")
		}
		sess.Disconnect()
		sess.Close()
	}
	s.emit(api.EvStateUpdated, api.StateUpdatedEvent{State: nil})
	return ok(req.ID, struct{}{})
}

func ptr[T any](v T) *T { return &v }

// validateCookies returns a bad_params message that never contains a cookie value.
func validateCookies(c map[string]string) string {
	for _, name := range []string{"c_user", "xs", "datr"} {
		if c[name] == "" {
			return "missing cookie " + name
		}
	}
	if len(c) > maxCookies {
		return "too many cookies"
	}
	for name, v := range c {
		if !cookieName.MatchString(name) {
			return "invalid cookie name"
		}
		if !validCookieValue(v) {
			return "invalid cookie " + name
		}
	}
	if !allDigits(c["c_user"]) || len(c["c_user"]) > 20 {
		return "invalid cookie c_user"
	}
	return ""
}

func validCookieValue(v string) bool {
	if len(v) < 1 || len(v) > maxCookieValue {
		return false
	}
	for i := 0; i < len(v); i++ {
		if b := v[i]; b <= 0x20 || b == 0x7f || b == ';' || b == ',' {
			return false
		}
	}
	return true
}

func allDigits(s string) bool {
	if s == "" {
		return false
	}
	for i := 0; i < len(s); i++ {
		if s[i] < '0' || s[i] > '9' {
			return false
		}
	}
	return true
}

func validUserAgent(ua string) bool {
	if len(ua) > maxUserAgent {
		return false
	}
	return !strings.ContainsFunc(ua, func(r rune) bool { return r < 0x20 || r > 0x7e })
}

// decodeParams decodes params into v, treating absent and null as an empty object.
func decodeParams(raw json.RawMessage, v any) bool {
	if isNull(raw) {
		raw = json.RawMessage("{}")
	}
	return json.Unmarshal(raw, v) == nil
}
