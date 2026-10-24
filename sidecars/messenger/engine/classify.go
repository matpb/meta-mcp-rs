package engine

import (
	"errors"

	"go.mau.fi/mautrix-meta/pkg/messagix/dgw"
	"go.mau.fi/mautrix-meta/pkg/messagix/httpclient"
	"go.mau.fi/mautrix-meta/pkg/messagix/types"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

type failureKind int

const (
	transient failureKind = iota
	fatalLoggedOut
	fatalConn
)

// failure is the engine's verdict on a library error.
type failure struct {
	kind   failureKind
	reason string
	code   string
	url    string
	action string
}

// errAccountDisabled is the Lightspeed error code Meta returns for a disabled account.
const errAccountDisabled = 1357053

func loggedOut(reason, code, url string) failure {
	return failure{kind: fatalLoggedOut, reason: reason, code: code, url: url, action: api.ActionRelogin}
}

// classify maps a library error to a failure. Order matters: ErrUserIDIsZero wraps ErrTokenInvalidated.
// ErrPleaseLogIn (1357001) is the invalid-login answer of an API call and counts as an invalidated token.
func classify(err error) failure {
	if err == nil {
		return failure{kind: transient, reason: "transient", code: api.CodeIO, action: api.ActionNone}
	}
	url := httpclient.GetErrorRedirectURL(err)
	var ls *types.ErrorResponse
	switch {
	case errors.Is(err, httpclient.ErrUserIDIsZero):
		return loggedOut("user_id_is_zero", api.CodeLoginFailed, "")
	case errors.Is(err, httpclient.ErrTokenInvalidated), errors.Is(err, types.ErrPleaseLogIn):
		return loggedOut("token_invalidated", api.CodeLoginFailed, "")
	case errors.Is(err, httpclient.ErrCheckpointRequired):
		return loggedOut("checkpoint_required", api.CodeCheckpointRequired, url)
	case errors.Is(err, httpclient.ErrChallengeRequired):
		return loggedOut("challenge_required", api.CodeChallengeRequired, url)
	case errors.Is(err, httpclient.ErrConsentRequired):
		return loggedOut("consent_required", api.CodeConsentRequired, url)
	case errors.Is(err, httpclient.ErrAccountSuspended):
		return loggedOut("account_suspended", api.CodeAccountSuspended, url)
	case errors.Is(err, httpclient.ErrRateLimited):
		return failure{kind: fatalConn, reason: "rate_limited", code: api.CodeRateLimited, action: api.ActionWait}
	case dgw.IsUnauthorized(err):
		return loggedOut("dgw_unauthorized", api.CodeLoginFailed, "")
	case errors.As(err, &ls) && ls.ErrorCode == errAccountDisabled:
		return loggedOut("account_disabled", api.CodeLoginFailed, "")
	case errors.Is(err, httpclient.ErrVersionIDNotFound):
		return failure{kind: fatalConn, reason: "permanent_error", code: api.CodeIO, action: api.ActionNone}
	}
	return failure{kind: transient, reason: "transient", code: api.CodeIO, action: api.ActionNone}
}

// clearsState reports whether a logged-out reason also forgets the stored session.
func (f failure) clearsState() bool {
	switch f.reason {
	case "token_invalidated", "user_id_is_zero", "dgw_unauthorized", "account_disabled":
		return true
	}
	return false
}

var failureText = map[string]string{
	"token_invalidated":   "the facebook session is no longer valid",
	"user_id_is_zero":     "the facebook session is no longer valid",
	"dgw_unauthorized":    "the facebook session was rejected",
	"account_disabled":    "the facebook account is disabled",
	"checkpoint_required": "facebook requires a checkpoint to be completed",
	"challenge_required":  "facebook requires a challenge to be completed",
	"consent_required":    "facebook requires consent to be given",
	"account_suspended":   "the facebook account is suspended",
	"rate_limited":        "facebook is rate limiting requests",
	"permanent_error":     "facebook connection failed",
}

// apiError is the fixed-text protocol error for a failure.
func (f failure) apiError() *api.Error {
	if f.kind == transient {
		return &api.Error{Code: api.CodeIO, Msg: "facebook unreachable"}
	}
	msg := failureText[f.reason]
	if msg == "" {
		msg = "facebook connection failed"
	}
	return &api.Error{Code: f.code, Msg: msg, Reason: f.reason, URL: f.url}
}
