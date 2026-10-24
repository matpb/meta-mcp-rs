package engine

import (
	"sync"
	"testing"

	"go.mau.fi/mautrix-meta/pkg/messagix/cookies"
	"go.mau.fi/mautrix-meta/pkg/messagix/types"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

func TestStateWatchEmitsOncePerChange(t *testing.T) {
	var mu sync.Mutex
	cur := api.State{Cookies: map[string]string{"c_user": "1", "xs": "a"}, FBID: "1"}
	var emitted []api.State
	w := newStateWatch(cur, func() api.State {
		mu.Lock()
		defer mu.Unlock()
		return api.State{Cookies: map[string]string{"c_user": cur.Cookies["c_user"], "xs": cur.Cookies["xs"]}, FBID: "1", E2EERegistered: cur.E2EERegistered}
	}, func(st api.State) { emitted = append(emitted, st) })

	w.check()
	w.check()
	if len(emitted) != 0 {
		t.Fatalf("no change, got %d emissions", len(emitted))
	}
	mu.Lock()
	cur.Cookies["xs"] = "b"
	mu.Unlock()
	w.check()
	w.check()
	if len(emitted) != 1 || emitted[0].Cookies["xs"] != "b" {
		t.Fatalf("one cookie change, got %+v", emitted)
	}
	mu.Lock()
	cur.E2EERegistered = true
	mu.Unlock()
	w.check()
	w.check()
	if len(emitted) != 2 || !emitted[1].E2EERegistered {
		t.Fatalf("one flag change, got %+v", emitted)
	}
}

func TestCookieMapAndSessionState(t *testing.T) {
	jar := &cookies.Cookies{Platform: types.Facebook}
	jar.UpdateValues(map[cookies.MetaCookieName]string{"c_user": "111", "xs": "x"})
	m := cookieMap(jar)
	if len(m) != 2 || m["xs"] != "x" {
		t.Fatalf("cookie map %v", m)
	}
	jar.Set("xs", "rotated")

	fc := newFakeClient(nil)
	s, rec := openTest(t, fc)
	s.jar.Set("xs", "rotated")
	s.watch.check()
	got := rec.named(api.EvStateUpdated)
	if len(got) != 1 {
		t.Fatalf("want one state_updated, got %d", len(got))
	}
	st := got[0].data.(api.StateUpdatedEvent).State
	if st == nil || st.Cookies["xs"] != "rotated" || st.FBID != "111" {
		t.Fatalf("state %+v", st)
	}
	s.watch.check()
	if len(rec.named(api.EvStateUpdated)) != 1 {
		t.Fatal("second check must stay quiet")
	}
}
