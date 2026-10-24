package engine

import "time"

const (
	backoffMin    = time.Second
	backoffMax    = 5 * time.Minute
	backoffStable = 2 * time.Minute
)

// backoff doubles from backoffMin to backoffMax; reset after a connection that stayed up backoffStable.
type backoff struct {
	cur time.Duration
}

func newBackoff() *backoff { return &backoff{cur: backoffMin} }

// Next returns the delay to wait now and doubles the following one.
func (b *backoff) Next() time.Duration {
	d := b.cur
	b.cur *= 2
	if b.cur > backoffMax {
		b.cur = backoffMax
	}
	return d
}

func (b *backoff) Reset() { b.cur = backoffMin }

// Stable reports whether a connection of this length counts as healthy.
func (b *backoff) Stable(up time.Duration) bool { return up >= backoffStable }
