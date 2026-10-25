package server

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

// M4: a second sidecar process on the same data_dir is refused in its first hello.
func TestSecondSidecarOnTheSameDataDirIsRefused(t *testing.T) {
	a := start(t)
	a.ready()
	a.helloOK("null")

	b := start(t)
	b.dir = a.dir
	b.ready()
	rep := b.call(b.helloLine(1, "null"))
	wantErr(t, rep, 1, "io")
	if rep.Error.Message != "another sidecar process holds this data_dir" {
		t.Fatalf("message %q", rep.Error.Message)
	}
	wantErr(t, b.call(b.req(2, "list_threads", "null")), 2, "not_initialised")
	wantOK(t, b.call(b.req(3, "ping", "null")), 3)

	// the lock goes with the first process
	wantOK(t, a.call(a.req(4, "shutdown", "null")), 4)
	if err := a.waitDone(); err != nil {
		t.Fatalf("Run returned %v", err)
	}
	b.helloOK("null")
}

func TestLockCoversHelloWithStateToo(t *testing.T) {
	a := start(t)
	a.ready()
	a.helloOK(goodState)
	b := start(t)
	b.dir = a.dir
	b.ready()
	wantErr(t, b.call(b.helloLine(1, goodState)), 1, "io")
	if len(b.f.opens) != 0 {
		t.Fatal("a refused hello must not open a session")
	}
}

func TestLockFileIsPrivateAndSurvivesLogout(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK(goodState)
	p := filepath.Join(h.dir, lockFileName)
	fi, err := os.Stat(p)
	if err != nil || posixModes && fi.Mode().Perm() != 0o600 {
		t.Fatalf("lock file: %v %v", fi, err)
	}
	wantOK(t, h.call(h.req(2, "logout", "null")), 2)
	if _, err := os.Stat(p); err != nil {
		t.Fatalf("logout must keep the lock file: %v", err)
	}
	other := start(t)
	other.dir = h.dir
	other.ready()
	wantErr(t, other.call(other.helloLine(1, "null")), 1, "io")
}

// A rejected or failed hello leaves no lock behind, so the same process can try again.
func TestFailedHelloReleasesTheLock(t *testing.T) {
	h := start(t)
	h.ready()
	h.f.openErr = errors.New("open failed")
	wantErr(t, h.call(h.helloLine(1, goodState)), 1, "internal")
	h.f.openErr = nil
	wantOK(t, h.call(h.helloLine(2, goodState)), 2)

	other := start(t)
	other.dir = h.dir
	other.ready()
	wantErr(t, other.call(other.helloLine(1, "null")), 1, "io")
}

// L7: a panic in an inline op becomes an internal reply, the process keeps serving and the lock is not leaked.
func TestInlineOpPanicBecomesInternalReply(t *testing.T) {
	h := start(t)
	h.ready()
	h.f.openPanic = true
	rep := h.call(h.helloLine(1, goodState))
	wantErr(t, rep, 1, "internal")
	if rep.Error.Message != "internal error" {
		t.Fatalf("message %q", rep.Error.Message)
	}
	wantOK(t, h.call(h.req(2, "ping", "null")), 2)
	h.f.mu.Lock()
	h.f.openPanic = false
	h.f.mu.Unlock()
	h.helloOK2("null", 3)
}

func (h *harness) helloOK2(state string, id int) map[string]any {
	h.t.Helper()
	return wantOK(h.t, h.call(h.helloLine(id, state)), uint64(id))
}

// M3: a failed deletion answers io but still clears the state and closes the session.
func TestLogoutWipeFailureAnswersIOAndStillClearsState(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK(goodState)
	h.f.mu.Lock()
	h.f.wipeErr = errors.New("disk on fire")
	h.f.mu.Unlock()
	rep := h.call(h.req(2, "logout", "null"))
	wantErr(t, rep, 2, "io")
	if rep.Error.Message != "failed to delete local session data" {
		t.Fatalf("message %q", rep.Error.Message)
	}
	if len(h.events) != 1 || h.events[0].Event != "state_updated" {
		t.Fatalf("events %+v", h.events)
	}
	if s := h.f.sessions[0]; s.loggedOut.Load() != 1 || s.closed.Load() != 1 {
		t.Fatalf("logout/close counts %d %d", s.loggedOut.Load(), s.closed.Load())
	}
	wantErr(t, h.call(h.req(3, "send_text", `{"thread_id":"1","text":"hi"}`)), 3, "not_logged_in")
}

// L6: the same rules as the WhatsApp sidecar.
func TestSharedOpRules(t *testing.T) {
	h := start(t)
	h.ready()
	wantOK(t, h.call(h.req(1, "ping", "null")), 1)
	wantErr(t, h.call(h.req(2, "send_text", `{"thread_id":"1","text":"x"}`)), 2, "not_initialised")
	wantErr(t, h.call(h.req(3, "frobnicate", "null")), 3, "not_initialised")
	h.helloOK("null")
	rep := h.call(h.helloLine(5, "null"))
	wantErr(t, rep, 5, "bad_params")
	if rep.Error.Message != "already initialised" {
		t.Fatalf("second hello: %q", rep.Error.Message)
	}
	rep = h.call(h.req(6, "frobnicate", `{"a":1}`))
	wantErr(t, rep, 6, "unsupported")
	if rep.Error.Message != "no such op" {
		t.Fatalf("unknown op: %q", rep.Error.Message)
	}
}

// A login cannot start while a logout is still deleting data, so the late state_updated null and the wipe
// can never hit the new session.
func TestLoginCannotStartWhileALogoutIsRunning(t *testing.T) {
	h := start(t)
	h.ready()
	entered, release := make(chan struct{}), make(chan struct{})
	h.f.logout = func(context.Context) error {
		close(entered)
		<-release
		return nil
	}
	h.helloOK(goodState)
	h.send(h.req(2, "logout", "null"))
	<-entered

	rep := h.call(h.req(3, "login_start", `{"method":"cookies"}`))
	wantErr(t, rep, 3, "bad_params")
	if rep.Error.Message != "logout in progress" {
		t.Fatalf("message %q", rep.Error.Message)
	}
	wantErr(t, h.call(h.req(4, "login_finish", fmt.Sprintf(`{"cookies":%s}`, goodCookies))), 4, "bad_params")
	if len(h.f.opens) != 1 {
		t.Fatalf("a refused login must not open a session, got %d opens", len(h.f.opens))
	}

	close(release)
	wantOK(t, h.nextReply(), 2)
	wantOK(t, h.call(h.req(5, "login_start", `{"method":"cookies"}`)), 5)
	wantOK(t, h.call(h.req(6, "login_finish", fmt.Sprintf(`{"cookies":%s}`, goodCookies))), 6)

	var states []string
	for _, ev := range h.events {
		if ev.Event != "state_updated" {
			continue
		}
		if strings.Contains(string(ev.Data), `"state":null`) {
			states = append(states, "null")
		} else {
			states = append(states, "set")
		}
	}
	if strings.Join(states, ",") != "null,set" {
		t.Fatalf("state_updated order %v, want the logout's null before the new login's state", states)
	}
}

// A session dropped after a fail-closed verdict is closed first, so its late events cannot write to the data dir.
func TestDroppedSessionIsClosedBeforeItIsDisconnected(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK(goodState)
	go h.f.emit(api.EvLoggedOut, api.LoggedOutEvent{Reason: "token_invalidated"})
	h.nextEvent()
	sess := h.f.sessions[0]
	deadline := time.Now().Add(2 * time.Second)
	for (sess.closed.Load() == 0 || sess.disconnected.Load() == 0) && time.Now().Before(deadline) {
		time.Sleep(5 * time.Millisecond)
	}
	if sess.closed.Load() != 1 || sess.disconnected.Load() != 1 {
		t.Fatalf("close %d disconnect %d", sess.closed.Load(), sess.disconnected.Load())
	}
	if sess.closeSeq.Load() == 0 || sess.closeSeq.Load() > sess.disconnectSeq.Load() {
		t.Fatalf("Close (%d) must come before Disconnect (%d)", sess.closeSeq.Load(), sess.disconnectSeq.Load())
	}
}

// A session dropped after a fail-closed verdict may still be finishing its work; logout waits for it
// before it deletes anything, so nothing is written to the data dir after the wipe.
func TestLogoutWaitsForADroppedSessionToClose(t *testing.T) {
	h := start(t)
	h.ready()
	entered, release := make(chan struct{}), make(chan struct{})
	h.f.closeHook = func() {
		select {
		case <-entered:
		default:
			close(entered)
		}
		<-release
	}
	h.helloOK(goodState)
	go h.f.emit(api.EvLoggedOut, api.LoggedOutEvent{Reason: "token_invalidated"})
	h.nextEvent()
	<-entered

	h.send(h.req(2, "logout", "null"))
	time.Sleep(60 * time.Millisecond)
	if w := h.f.wipeCalls(); len(w) != 0 {
		t.Fatalf("the data dir was wiped while a dropped session was still closing: %v", w)
	}
	close(release)
	wantOK(t, h.nextReply(), 2)
	if w := h.f.wipeCalls(); len(w) != 1 {
		t.Fatalf("wipes %v", w)
	}
}
