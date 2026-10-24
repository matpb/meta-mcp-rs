package engine

import (
	"maps"
	"sync"
	"time"

	"go.mau.fi/mautrix-meta/pkg/messagix/cookies"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

const cookieTick = 60 * time.Second

// stateWatch emits state_updated whenever the cookies or the E2EE flag differ from the last emitted state.
type stateWatch struct {
	mu   sync.Mutex
	last api.State
	read func() api.State
	emit func(api.State)
}

func newStateWatch(initial api.State, read func() api.State, emit func(api.State)) *stateWatch {
	initial.Cookies = maps.Clone(initial.Cookies)
	return &stateWatch{last: initial, read: read, emit: emit}
}

// check emits at most one state_updated per change.
func (w *stateWatch) check() {
	cur := w.read()
	w.mu.Lock()
	if maps.Equal(cur.Cookies, w.last.Cookies) && cur.E2EERegistered == w.last.E2EERegistered {
		w.mu.Unlock()
		return
	}
	w.last = cur
	w.mu.Unlock()
	w.emit(cur)
}

// reset forgets the baseline without emitting (used when the session is wiped).
func (w *stateWatch) reset(st api.State) {
	w.mu.Lock()
	w.last = st
	w.mu.Unlock()
}

// cookieMap copies the live cookie jar into the wire form.
func cookieMap(c *cookies.Cookies) map[string]string {
	out := map[string]string{}
	if c == nil {
		return out
	}
	for k, v := range c.GetAll() {
		out[string(k)] = v
	}
	return out
}
