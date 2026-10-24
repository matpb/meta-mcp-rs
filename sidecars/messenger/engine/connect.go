package engine

import (
	"context"
	"sync"
	"sync/atomic"
	"time"

	"go.mau.fi/mautrix-meta/pkg/messagix"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

// maxPageFailures is how many page loads in a row may fail without a verdict before the loop stops.
const maxPageFailures = 6

// Tunables; tests lower them.
var (
	periodicReconnect = 20 * time.Hour
	minFullReconnect  = time.Hour
	cookieInterval    = cookieTick
)

// cycle is one connected period: a client, its event queue and the goroutines serving them.
type cycle struct {
	ctx      context.Context
	cancel   context.CancelFunc
	client   metaClient
	q        *eventQueue
	initial  *table.LSTable
	initOnce sync.Once
	e2eeOnce sync.Once
	upSince  atomic.Int64
	wg       sync.WaitGroup
	ended    chan struct{}
	endOnce  sync.Once
}

func (c *cycle) end() { c.endOnce.Do(func() { close(c.ended) }) }

type cycleOutcome int

const (
	outStop cycleOutcome = iota
	outRetry
	outRecycle
)

// Connect starts the realtime and E2EE connections in the background; it returns right away.
func (s *session) Connect(ctx context.Context) error {
	for {
		s.mu.Lock()
		switch {
		case s.closed.Load():
			s.mu.Unlock()
			return api.Errorf(api.CodeNotLoggedIn, "session closed")
		case s.loggedOut || s.wiped || len(s.jar.GetMissingCookieNames()) > 0:
			s.mu.Unlock()
			return api.Errorf(api.CodeNotLoggedIn, "not logged in")
		case s.blocked != "":
			reason := s.blocked
			s.mu.Unlock()
			return api.Errorf(api.CodeNotConnected, "sending is blocked (%s); restart the sidecar after the cause is resolved", reason)
		case s.runCancel != nil:
			s.mu.Unlock()
			return nil
		case s.runDone != nil:
			done := s.runDone
			s.mu.Unlock()
			select {
			case <-done:
			case <-time.After(stopWait):
				return api.Errorf(api.CodeNotConnected, "previous connection is still shutting down")
			case <-ctx.Done():
				return api.Errorf(api.CodeIO, "request cancelled")
			}
			continue
		}
		runCtx, cancel := context.WithCancel(context.Background())
		done := make(chan struct{})
		s.runCancel, s.runDone = cancel, done
		s.mu.Unlock()
		go s.run(runCtx, done)
		return nil
	}
}

func (s *session) run(ctx context.Context, done chan struct{}) {
	defer func() {
		s.mu.Lock()
		if s.runDone == done {
			s.runCancel, s.runDone = nil, nil
		}
		s.mu.Unlock()
		close(done)
	}()
	s.mu.Lock()
	s.pageFails = 0
	s.mu.Unlock()
	bo := newBackoff()
	for ctx.Err() == nil {
		start := time.Now()
		out := outRetry
		s.guard("connect cycle", func() { out = s.runCycle(ctx) })
		if bo.Stable(time.Since(start)) {
			bo.Reset()
		}
		switch out {
		case outStop:
			return
		case outRetry:
			select {
			case <-time.After(bo.Next()):
			case <-ctx.Done():
				return
			}
		}
	}
}

func (s *session) resetGroups() {
	s.listMu.Lock()
	s.groups = map[int64]*groupFetch{}
	s.listMu.Unlock()
}

// runCycle loads the page, connects and waits until the cycle must end.
func (s *session) runCycle(ctx context.Context) cycleOutcome {
	s.emitConn(api.ConnConnecting, "", "")
	s.resetGroups()
	cl, tbl, ok := s.takeValidated()
	if !ok {
		var err error
		cl, tbl, _, err = s.loadPage(ctx)
		if err != nil {
			if ctx.Err() != nil {
				return outStop
			}
			f := classify(err)
			if ae, isAPI := err.(*api.Error); isAPI && ae == errAccountMismatch {
				f = loggedOut("account_mismatch", api.CodeLoginFailed, "")
			}
			if s.tooManyPageFailures(f) {
				s.emitConn(api.ConnFatal, "repeated_failures", api.ActionRelogin)
				return outStop
			}
			return s.cycleFailure(f)
		}
		s.mu.Lock()
		s.pageFails = 0
		s.mu.Unlock()
	}
	s.mu.Lock()
	s.lastFull = time.Now()
	s.mu.Unlock()

	cctx, cancel := context.WithCancel(ctx)
	cyc := &cycle{ctx: cctx, cancel: cancel, client: cl, q: newEventQueue(), initial: tbl, ended: make(chan struct{})}
	s.mu.Lock()
	s.cyc = cyc
	s.mu.Unlock()
	defer s.teardown(cyc)

	cl.SetEventHandler(func(_ context.Context, evt any) {
		if cctx.Err() == nil {
			cyc.q.push(evt)
		}
	})
	cyc.wg.Add(2)
	go s.consume(cyc)
	go s.watchCookies(cyc)
	if err := cl.Connect(cctx); err != nil {
		if ctx.Err() != nil {
			return outStop
		}
		return s.cycleFailure(classify(err))
	}

	timer := time.NewTimer(periodicReconnect)
	defer timer.Stop()
	for {
		select {
		case <-ctx.Done():
			return outStop
		case <-cyc.ended:
			return outStop
		case <-timer.C:
			s.log.Info().Msg("periodic reconnect")
			return outRecycle
		case force := <-s.recycle:
			s.mu.Lock()
			recent := time.Since(s.lastFull) < minFullReconnect
			s.mu.Unlock()
			if force || !recent {
				s.log.Info().Msg("full reconnect requested")
				return outRecycle
			}
		}
	}
}

// tooManyPageFailures counts consecutive page loads that failed without a verdict and reports when the cap is hit.
func (s *session) tooManyPageFailures(f failure) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if f.kind != transient {
		s.pageFails = 0
		return false
	}
	s.pageFails++
	return s.pageFails >= maxPageFailures
}

// cycleFailure applies a failure that happened while starting a cycle.
func (s *session) cycleFailure(f failure) cycleOutcome {
	switch f.kind {
	case fatalLoggedOut:
		s.failClosed(f)
		return outStop
	case fatalConn:
		s.emitConn(api.ConnFatal, f.reason, f.action)
		return outStop
	}
	s.emitConn(api.ConnDisconnected, "transient", "")
	return outRetry
}

// requestRecycle asks the loop for a full reconnect; unforced requests honour the minimum interval.
func (s *session) requestRecycle(force bool) {
	select {
	case s.recycle <- force:
	default:
	}
}

// failClosed reports a logout once, forgets the session when the reason demands it and stops the loop.
func (s *session) failClosed(f failure) {
	s.mu.Lock()
	if s.loggedOut {
		s.mu.Unlock()
		return
	}
	s.loggedOut = true
	cancel := s.runCancel
	s.mu.Unlock()
	s.log.Info().Str("reason", f.reason).Msg("logged out")
	s.emit(api.EvLoggedOut, api.LoggedOutEvent{Reason: f.reason, URL: f.url})
	if f.clearsState() {
		s.emit(api.EvStateUpdated, api.StateUpdatedEvent{State: nil})
	}
	if cancel != nil {
		cancel()
	}
}

// handleFailure applies a failure raised by an event of the running cycle.
func (s *session) handleFailure(cyc *cycle, f failure) {
	switch f.kind {
	case fatalLoggedOut:
		s.failClosed(f)
		cyc.end()
	case fatalConn:
		s.emitConn(api.ConnFatal, f.reason, f.action)
		cyc.end()
	default:
		s.emitConn(api.ConnDisconnected, "transient", "")
	}
}

// teardown disconnects both clients and stops the cycle goroutines; the library handler stays set and drops events once cancelled.
func (s *session) teardown(cyc *cycle) {
	cyc.cancel()
	cyc.client.Disconnect()
	s.stopE2EE()
	cyc.q.close()
	cyc.wg.Wait()
	s.mu.Lock()
	if s.cyc == cyc {
		s.cyc = nil
	}
	s.mu.Unlock()
}

func (s *session) watchCookies(cyc *cycle) {
	defer cyc.wg.Done()
	t := time.NewTicker(cookieInterval)
	defer t.Stop()
	for {
		select {
		case <-t.C:
			s.watch.check()
		case <-cyc.ctx.Done():
			return
		}
	}
}

func (s *session) consume(cyc *cycle) {
	defer cyc.wg.Done()
	for {
		evt, ok := cyc.q.pop(cyc.ctx)
		if !ok {
			return
		}
		s.safeHandle(cyc, evt)
	}
}

func (s *session) safeHandle(cyc *cycle, evt any) {
	s.guard("event handler", func() { s.handleEvent(cyc, evt) })
}

func (s *session) handleEvent(cyc *cycle, evt any) {
	switch e := evt.(type) {
	case *table.LSTable:
		s.processTable(e, modeLive)
		cyc.client.PostHandlePublishResponse(e)
	case *messagix.ConnectedEvent:
		s.onConnected(cyc)
	case *messagix.ReconnectedEvent:
		s.onConnected(cyc)
	case *messagix.TransientDisconnectEvent:
		cyc.upSince.Store(0)
		s.emitConn(api.ConnDisconnected, "transient", "")
	case *messagix.PermanentErrorEvent:
		f := classify(e.Err)
		if f.kind == transient {
			f = failure{kind: fatalConn, reason: "permanent_error", code: api.CodeIO, action: api.ActionNone}
		}
		s.handleFailure(cyc, f)
	}
}

func (s *session) onConnected(cyc *cycle) {
	cyc.upSince.Store(time.Now().UnixNano())
	s.emitConn(api.ConnConnected, "", "")
	cyc.initOnce.Do(func() {
		if cyc.initial != nil {
			s.processTable(cyc.initial, modeInitial)
			cyc.initial = nil
		}
	})
	cyc.e2eeOnce.Do(func() {
		cyc.wg.Add(1)
		go func() {
			defer cyc.wg.Done()
			s.startE2EE(cyc)
		}()
	})
}

// connected reports whether the realtime socket is up.
func (s *session) connected() bool {
	cl := s.messagix()
	return cl != nil && cl.IsConnected()
}
