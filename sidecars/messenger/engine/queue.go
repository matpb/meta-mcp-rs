package engine

import (
	"context"
	"sync"
)

// eventQueue is an unbounded FIFO so the library's handler never blocks.
type eventQueue struct {
	mu     sync.Mutex
	items  []any
	wake   chan struct{}
	closed bool
}

func newEventQueue() *eventQueue { return &eventQueue{wake: make(chan struct{}, 1)} }

func (q *eventQueue) push(evt any) {
	q.mu.Lock()
	if q.closed {
		q.mu.Unlock()
		return
	}
	q.items = append(q.items, evt)
	q.mu.Unlock()
	select {
	case q.wake <- struct{}{}:
	default:
	}
}

func (q *eventQueue) close() {
	q.mu.Lock()
	q.closed = true
	q.mu.Unlock()
	select {
	case q.wake <- struct{}{}:
	default:
	}
}

// pop blocks for the next event; false once ctx ends or the queue is closed and drained.
func (q *eventQueue) pop(ctx context.Context) (any, bool) {
	for {
		q.mu.Lock()
		if len(q.items) > 0 {
			evt := q.items[0]
			q.items[0] = nil
			q.items = q.items[1:]
			q.mu.Unlock()
			return evt, true
		}
		closed := q.closed
		q.mu.Unlock()
		if closed {
			return nil, false
		}
		select {
		case <-q.wake:
		case <-ctx.Done():
			return nil, false
		}
	}
}
