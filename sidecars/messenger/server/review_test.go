package server

import (
	"bufio"
	"context"
	"io"
	"testing"

	"github.com/rs/zerolog"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

// The tests below are the reviewer's regressions for M3, inverted: logout wipes through the factory and reports failure.

// After a fail-closed logged_out the server holds no session, but logout still wipes the data dir.
func TestReviewLogoutAfterLoggedOutWipesNothing(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK(goodState)
	go h.f.emit(api.EvLoggedOut, api.LoggedOutEvent{Reason: "token_invalidated"})
	h.nextEvent() // logged_out
	wantOK(t, h.call(h.req(2, "logout", "null")), 2)
	if w := h.f.wipeCalls(); len(w) != 1 || w[0] != h.dir {
		t.Errorf("logout after logged_out wiped %v, want [%s]", w, h.dir)
	}
}

// A hello with state null has no session object, and logout still wipes leftovers of an earlier run.
func TestReviewLogoutWithoutSessionAfterRestart(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK("null")
	wantOK(t, h.call(h.req(2, "logout", "null")), 2)
	if w := h.f.wipeCalls(); len(w) != 1 || w[0] != h.dir {
		t.Errorf("logout without a session wiped %v, want [%s]", w, h.dir)
	}
}

type failingLogoutFactory struct{ *fakeFactory }

type failingSession struct{ *fakeSession }

func (s failingSession) Logout(context.Context) error {
	return api.Errorf(api.CodeIO, "failed to delete local session data")
}

func (f failingLogoutFactory) Open(st api.State, dir string, sink api.Sink) (api.Session, error) {
	s, err := f.fakeFactory.Open(st, dir, sink)
	if err != nil {
		return nil, err
	}
	return failingSession{s.(*fakeSession)}, nil
}

// A failed deletion is an io error, not ok, and the daemon's state is cleared all the same.
func TestReviewLogoutReportsOKWhenWipeFails(t *testing.T) {
	h2 := startWithFactory(t, failingLogoutFactory{&fakeFactory{}})
	h2.ready()
	h2.helloOK(goodState)
	rep := h2.call(h2.req(2, "logout", "null"))
	wantErr(t, rep, 2, "io")
	if rep.Error.Message != "failed to delete local session data" {
		t.Errorf("message %q", rep.Error.Message)
	}
	if len(h2.events) == 0 || h2.events[len(h2.events)-1].Event != "state_updated" {
		t.Errorf("state_updated must still be emitted: %+v", h2.events)
	}
}

func startWithFactory(t *testing.T, ff failingLogoutFactory) *harness {
	t.Helper()
	inR, inW := io.Pipe()
	outR, outW := io.Pipe()
	h := &harness{t: t, toSrv: inW, fromSr: bufio.NewReader(outR), done: make(chan error, 1), f: ff.fakeFactory, dir: t.TempDir()}
	go func() {
		err := run(context.Background(), inR, outW, zerolog.Nop(), ff, nil)
		outW.Close()
		h.done <- err
	}()
	t.Cleanup(func() { inW.Close() })
	return h
}
