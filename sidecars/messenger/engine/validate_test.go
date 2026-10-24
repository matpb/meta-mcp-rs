package engine

import (
	"errors"
	"fmt"
	"testing"

	"go.mau.fi/mautrix-meta/pkg/messagix/httpclient"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

func TestValidateSuccess(t *testing.T) {
	fc := newFakeClient(nil)
	s, rec := openTest(t, fc)
	acct, err := s.Validate(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	if acct.ID != "111" || acct.Name != testName {
		t.Fatalf("account %+v", acct)
	}
	if got := rec.named(api.EvStateUpdated); len(got) != 0 {
		t.Fatalf("unchanged cookies must not emit state_updated: %v", got)
	}
	if loads, _, _ := fc.counts(); loads != 1 {
		t.Fatalf("loads %d", loads)
	}
	if st := s.State(); st.FBID != "111" || st.Cookies["xs"] != "xs-secret" {
		t.Fatalf("state %+v", st)
	}
}

func TestValidateErrors(t *testing.T) {
	cases := []struct {
		name   string
		err    error
		code   string
		reason string
		url    string
	}{
		{"checkpoint", httpclient.RedirectedError{Type: httpclient.ErrCheckpointRequired, URL: "https://x/checkpoint/"}, api.CodeCheckpointRequired, "checkpoint_required", "https://x/checkpoint/"},
		{"token", fmt.Errorf("failed to load inbox: %w", httpclient.ErrTokenInvalidated), api.CodeLoginFailed, "token_invalidated", ""},
		{"transient", errors.New("dial tcp: refused"), api.CodeIO, "", ""},
		{"rate limited", httpclient.ErrRateLimited, api.CodeRateLimited, "rate_limited", ""},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			fc := newFakeClient(nil)
			fc.loadErr = tc.err
			s, rec := openTest(t, fc)
			_, err := s.Validate(t.Context())
			ae, ok := err.(*api.Error)
			if !ok || ae.Code != tc.code || ae.Reason != tc.reason || ae.URL != tc.url {
				t.Fatalf("error %#v", err)
			}
			if len(rec.all()) != 0 {
				t.Fatalf("validate must not emit events: %v", rec.names())
			}
		})
	}
}

func TestValidateAccountMismatch(t *testing.T) {
	fc := newFakeClient(nil)
	fc.user = fakeUser{id: 999, name: "Other"}
	s, _ := openTest(t, fc)
	_, err := s.Validate(t.Context())
	ae, ok := err.(*api.Error)
	if !ok || ae.Code != api.CodeLoginFailed {
		t.Fatalf("error %#v", err)
	}
}

func TestValidateTableReusedByConnect(t *testing.T) {
	fc := newFakeClient(nil)
	s, rec := openTest(t, fc)
	if _, err := s.Validate(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := s.Connect(t.Context()); err != nil {
		t.Fatal(err)
	}
	rec.wait(t, "connecting", func(ev []recordedEvent) bool {
		_, connects, _ := fc.counts()
		return connects == 1
	})
	if loads, _, _ := fc.counts(); loads != 1 {
		t.Fatalf("connect within 5 minutes must reuse the validated page, loads=%d", loads)
	}
}
