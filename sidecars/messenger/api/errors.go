package api

import (
	"errors"
	"fmt"
)

// Error codes of protocol 1.
const (
	CodeNotInitialised     = "not_initialised"
	CodeProtocolMismatch   = "protocol_mismatch"
	CodeBadParams          = "bad_params"
	CodeNotLoggedIn        = "not_logged_in"
	CodeAlreadyLoggedIn    = "already_logged_in"
	CodeLoginFailed        = "login_failed"
	CodeCheckpointRequired = "checkpoint_required"
	CodeChallengeRequired  = "challenge_required"
	CodeConsentRequired    = "consent_required"
	CodeAccountSuspended   = "account_suspended"
	CodeRateLimited        = "rate_limited"
	CodeNotConnected       = "not_connected"
	CodeThreadNotFound     = "thread_not_found"
	CodeMessageNotFound    = "message_not_found"
	CodeAttachmentExpired  = "attachment_expired"
	CodeSendFailed         = "send_failed"
	CodeSendUnknown        = "send_unknown"
	CodeUnsupported        = "unsupported"
	CodeIO                 = "io"
	CodeInternal           = "internal"
)

// Error is a protocol error; Msg must never carry a cookie, token, key or message body.
type Error struct {
	Code string
	Msg  string
	// Reason and URL are set for fail-closed account errors (checkpoint and friends).
	Reason string
	URL    string
}

func (e *Error) Error() string { return e.Code + ": " + e.Msg }

// Errorf builds an *Error with a formatted message.
func Errorf(code, format string, args ...any) *Error {
	return &Error{Code: code, Msg: fmt.Sprintf(format, args...)}
}

// AsError returns err as *Error, wrapping anything else as internal with a fixed message.
func AsError(err error) *Error {
	var e *Error
	if errors.As(err, &e) {
		return e
	}
	return &Error{Code: CodeInternal, Msg: "internal error"}
}
