package engine

import (
	"sync"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

// sendWaiter routes server answers for optimistic sends to the call waiting on that OTID.
// The zero value is ready to use.
type sendWaiter struct {
	mu      sync.Mutex
	pending map[string]chan api.SendOutcome
}

// register starts waiting for an OTID; the channel receives at most one outcome.
func (w *sendWaiter) register(otid string) <-chan api.SendOutcome {
	ch := make(chan api.SendOutcome, 1)
	w.mu.Lock()
	if w.pending == nil {
		w.pending = map[string]chan api.SendOutcome{}
	}
	w.pending[otid] = ch
	w.mu.Unlock()
	return ch
}

// resolve delivers the first outcome per registered OTID and ignores unknown ones.
func (w *sendWaiter) resolve(outcomes []api.SendOutcome) {
	w.mu.Lock()
	defer w.mu.Unlock()
	for _, o := range outcomes {
		ch, ok := w.pending[o.OTID]
		if !ok {
			continue
		}
		ch <- o
		delete(w.pending, o.OTID)
	}
}

// cancel forgets an OTID without delivering anything.
func (w *sendWaiter) cancel(otid string) {
	w.mu.Lock()
	delete(w.pending, otid)
	w.mu.Unlock()
}

// size is the number of OTIDs still waiting.
func (w *sendWaiter) size() int {
	w.mu.Lock()
	defer w.mu.Unlock()
	return len(w.pending)
}
