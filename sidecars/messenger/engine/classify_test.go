package engine

import (
	"context"
	"errors"
	"fmt"
	"net"
	"testing"
	"time"

	"github.com/coder/websocket"
	"go.mau.fi/mautrix-meta/pkg/messagix/dgw"
	"go.mau.fi/mautrix-meta/pkg/messagix/httpclient"
	"go.mau.fi/mautrix-meta/pkg/messagix/types"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

func TestClassify(t *testing.T) {
	checkpointURL := "https://www.facebook.com/checkpoint/1234/"
	cases := []struct {
		name   string
		err    error
		kind   failureKind
		reason string
		code   string
		url    string
		action string
	}{
		{"token invalidated", httpclient.ErrTokenInvalidated, fatalLoggedOut, "token_invalidated", api.CodeLoginFailed, "", api.ActionRelogin},
		{"token redirect", httpclient.ErrTokenInvalidatedRedirect, fatalLoggedOut, "token_invalidated", api.CodeLoginFailed, "", api.ActionRelogin},
		{"user id zero", httpclient.ErrUserIDIsZero, fatalLoggedOut, "user_id_is_zero", api.CodeLoginFailed, "", api.ActionRelogin},
		{"checkpoint", httpclient.RedirectedError{Type: httpclient.ErrCheckpointRequired, URL: checkpointURL}, fatalLoggedOut, "checkpoint_required", api.CodeCheckpointRequired, checkpointURL, api.ActionRelogin},
		{"challenge", httpclient.ErrChallengeRequired, fatalLoggedOut, "challenge_required", api.CodeChallengeRequired, "", api.ActionRelogin},
		{"consent", httpclient.ErrConsentRequired, fatalLoggedOut, "consent_required", api.CodeConsentRequired, "", api.ActionRelogin},
		{"suspended", httpclient.ErrAccountSuspended, fatalLoggedOut, "account_suspended", api.CodeAccountSuspended, "", api.ActionRelogin},
		{"rate limited", httpclient.ErrRateLimited, fatalConn, "rate_limited", api.CodeRateLimited, "", api.ActionWait},
		{"dgw sentinel", dgw.ErrUnauthorized, fatalLoggedOut, "dgw_unauthorized", api.CodeLoginFailed, "", api.ActionRelogin},
		{"dgw close status", websocket.CloseError{Code: dgw.CloseStatusUnauthorized}, fatalLoggedOut, "dgw_unauthorized", api.CodeLoginFailed, "", api.ActionRelogin},
		{"account disabled", &types.ErrorResponse{ErrorCode: 1357053}, fatalLoggedOut, "account_disabled", api.CodeLoginFailed, "", api.ActionRelogin},
		{"please log in", types.ErrPleaseLogIn, fatalLoggedOut, "token_invalidated", api.CodeLoginFailed, "", api.ActionRelogin},
		{"please log in by code", &types.ErrorResponse{ErrorCode: 1357001}, fatalLoggedOut, "token_invalidated", api.CodeLoginFailed, "", api.ActionRelogin},
		{"wrapped please log in", fmt.Errorf("send failed: %w", types.ErrPleaseLogIn), fatalLoggedOut, "token_invalidated", api.CodeLoginFailed, "", api.ActionRelogin},
		{"please reload page stays transient", types.ErrPleaseReloadPage, transient, "transient", api.CodeIO, "", api.ActionNone},
		{"other ls error", &types.ErrorResponse{ErrorCode: 1357999}, transient, "transient", api.CodeIO, "", api.ActionNone},
		{"version id", httpclient.ErrVersionIDNotFound, fatalConn, "permanent_error", api.CodeIO, "", api.ActionNone},
		{"context canceled", context.Canceled, transient, "transient", api.CodeIO, "", api.ActionNone},
		{"deadline", context.DeadlineExceeded, transient, "transient", api.CodeIO, "", api.ActionNone},
		{"net error", &net.OpError{Op: "dial", Err: errors.New("refused")}, transient, "transient", api.CodeIO, "", api.ActionNone},
		{"max retries", httpclient.ErrMaxRetriesReached, transient, "transient", api.CodeIO, "", api.ActionNone},
		{"unexpected status", fmt.Errorf("%w 503", httpclient.ErrUnexpectedError), transient, "transient", api.CodeIO, "", api.ActionNone},
		{"plain", errors.New("boom"), transient, "transient", api.CodeIO, "", api.ActionNone},
		{"wrapped token", fmt.Errorf("failed to load inbox: %w", httpclient.ErrTokenInvalidated), fatalLoggedOut, "token_invalidated", api.CodeLoginFailed, "", api.ActionRelogin},
		{"wrapped zero beats token", fmt.Errorf("failed: %w", httpclient.ErrUserIDIsZero), fatalLoggedOut, "user_id_is_zero", api.CodeLoginFailed, "", api.ActionRelogin},
		{"joined", errors.Join(errors.New("a"), httpclient.ErrConsentRequired), fatalLoggedOut, "consent_required", api.CodeConsentRequired, "", api.ActionRelogin},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := classify(tc.err)
			if got.kind != tc.kind || got.reason != tc.reason || got.code != tc.code || got.url != tc.url || got.action != tc.action {
				t.Fatalf("classify = %+v", got)
			}
		})
	}
}

func TestFailureAPIError(t *testing.T) {
	e := classify(httpclient.RedirectedError{Type: httpclient.ErrCheckpointRequired, URL: "https://x/checkpoint/"}).apiError()
	if e.Code != api.CodeCheckpointRequired || e.Reason != "checkpoint_required" || e.URL != "https://x/checkpoint/" || e.Msg == "" {
		t.Fatalf("checkpoint error %+v", e)
	}
	e = classify(errors.New("secret detail")).apiError()
	if e.Code != api.CodeIO || e.Msg != "facebook unreachable" {
		t.Fatalf("transient error %+v", e)
	}
	for _, reason := range []string{"token_invalidated", "user_id_is_zero", "dgw_unauthorized", "account_disabled"} {
		if !(failure{reason: reason}).clearsState() {
			t.Fatalf("%s must clear state", reason)
		}
	}
	for _, reason := range []string{"checkpoint_required", "challenge_required", "consent_required", "account_suspended"} {
		if (failure{reason: reason}).clearsState() {
			t.Fatalf("%s must keep state", reason)
		}
	}
}

func TestBackoffSequence(t *testing.T) {
	b := newBackoff()
	want := []time.Duration{1, 2, 4, 8, 16, 32, 64, 128, 256}
	for i, w := range want {
		if got := b.Next(); got != w*time.Second {
			t.Fatalf("step %d: %v, want %v", i, got, w*time.Second)
		}
	}
	for i := 0; i < 3; i++ {
		if got := b.Next(); got != backoffMax {
			t.Fatalf("cap: %v", got)
		}
	}
	b.Reset()
	if got := b.Next(); got != time.Second {
		t.Fatalf("after reset: %v", got)
	}
	if b.Stable(time.Minute) || !b.Stable(2*time.Minute) {
		t.Fatal("stable threshold is two minutes")
	}
}
