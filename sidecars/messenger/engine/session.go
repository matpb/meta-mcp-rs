package engine

import (
	"context"
	"strconv"
	"sync"
	"sync/atomic"
	"time"

	"github.com/rs/zerolog"
	"go.mau.fi/mautrix-meta/pkg/messagix/cookies"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
	"go.mau.fi/whatsmeow"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
	"github.com/matpb/meta-mcp-rs/sidecars/messenger/e2ee"
)

// validReuse is how long a table loaded by Validate is reused by the first Connect.
const validReuse = 5 * time.Minute

type validatedPage struct {
	tbl *table.LSTable
	at  time.Time
}

type groupFetch struct {
	prevMin   int64
	exhausted bool
}

type session struct {
	f         *factory
	log       zerolog.Logger
	sink      api.Sink
	dataDir   string
	userAgent string
	fbid      string
	jar       *cookies.Cookies
	me        atomic.Int64
	closed    atomic.Bool

	reg   *registry
	names *contacts
	hist  *msgCache
	watch *stateWatch

	procMu sync.Mutex
	sends  sendWaiter

	openMu sync.Mutex
	people vettedPeople

	collMu     sync.Mutex
	collectors map[string]*collector

	listMu sync.Mutex
	groups map[int64]*groupFetch

	// mu guards the fields below.
	mu             sync.Mutex
	client         metaClient
	clientLoaded   bool
	validated      *validatedPage
	loggedOut      bool
	wiped          bool
	blocked        string
	pageFails      int
	cyc            *cycle
	runCancel      context.CancelFunc
	runDone        chan struct{}
	e2eeRegistered bool
	e2eeRecoveries int
	lastFull       time.Time
	recycle        chan bool

	// e2mu guards the encrypted side.
	e2mu sync.Mutex
	st   *e2ee.Store
	wa   *whatsmeow.Client
}

func newSession(f *factory, state api.State, dataDir string, sink api.Sink, jar *cookies.Cookies, me int64) *session {
	s := &session{
		f:              f,
		log:            f.log.With().Str("component", "engine").Logger(),
		sink:           sink,
		dataDir:        dataDir,
		userAgent:      state.UserAgent,
		fbid:           state.FBID,
		jar:            jar,
		names:          newContacts(),
		hist:           newMsgCache(),
		collectors:     map[string]*collector{},
		groups:         map[int64]*groupFetch{},
		e2eeRegistered: state.E2EERegistered,
		recycle:        make(chan bool, 1),
	}
	s.me.Store(me)
	s.reg = newRegistry(dataDir, state.FBID, me, s.names.get)
	s.watch = newStateWatch(state, s.State, func(st api.State) {
		s.emit(api.EvStateUpdated, api.StateUpdatedEvent{State: &st})
	})
	return s
}

func (s *session) emit(name string, data any) {
	if s.sink == nil || s.closed.Load() {
		return
	}
	s.sink(name, data)
}

// emitConn reports a connection change; after a fatal account signal only fatal events follow, so the
// daemon keeps the state it already holds.
func (s *session) emitConn(state, reason, action string) {
	if state != api.ConnFatal && s.blockedReason() != "" {
		return
	}
	s.emit(api.EvConnection, api.ConnectionEvent{State: state, Reason: reason, Action: action})
}

// State is the current persistable session; cookies reflect rotations since login.
func (s *session) State() api.State {
	s.mu.Lock()
	wiped, reg := s.wiped, s.e2eeRegistered
	s.mu.Unlock()
	st := api.State{Cookies: map[string]string{}, UserAgent: s.userAgent, FBID: s.fbid, E2EERegistered: reg}
	if !wiped {
		st.Cookies = cookieMap(s.jar)
	}
	return st
}

// clientForLoad returns a client that has not loaded the messages page yet, creating one when needed.
func (s *session) clientForLoad() metaClient {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.client == nil || s.clientLoaded {
		s.client = s.f.newClient(s.jar, s.log.With().Str("component", "messagix").Logger())
		s.clientLoaded = false
	}
	return s.client
}

// messagix returns the active client, or the last built one, or nil.
func (s *session) messagix() metaClient {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.cyc != nil {
		return s.cyc.client
	}
	return s.client
}

func (s *session) loadPage(ctx context.Context) (metaClient, *table.LSTable, string, error) {
	cl := s.clientForLoad()
	user, tbl, err := cl.LoadMessagesPage(ctx)
	if err != nil {
		return nil, nil, "", err
	}
	if user == nil {
		return nil, nil, "", api.Errorf(api.CodeIO, "facebook unreachable")
	}
	s.mu.Lock()
	s.clientLoaded = true
	s.mu.Unlock()
	me := user.GetFBID()
	if me == 0 || strconv.FormatInt(me, 10) != s.fbid {
		return nil, nil, "", errAccountMismatch
	}
	s.me.Store(me)
	s.reg.setMe(me)
	s.names.set(me, user.GetName())
	s.watch.check()
	return cl, tbl, user.GetName(), nil
}

var errAccountMismatch = &api.Error{Code: api.CodeLoginFailed, Msg: "the cookies belong to a different account", Reason: "account_mismatch"}

// Validate loads the messages page once and verifies the account.
func (s *session) Validate(ctx context.Context) (api.Account, error) {
	if s.closed.Load() {
		return api.Account{}, api.Errorf(api.CodeNotLoggedIn, "session closed")
	}
	cl, tbl, name, err := s.loadPage(ctx)
	if err != nil {
		if e, ok := err.(*api.Error); ok {
			return api.Account{}, e
		}
		return api.Account{}, classify(err).apiError()
	}
	s.mu.Lock()
	s.client = cl
	s.validated = &validatedPage{tbl: tbl, at: time.Now()}
	s.mu.Unlock()
	return api.Account{ID: s.fbid, Name: name}, nil
}

// takeValidated hands the Validate result to the first Connect when it is fresh.
func (s *session) takeValidated() (metaClient, *table.LSTable, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	v, cl := s.validated, s.client
	s.validated = nil
	if v == nil || cl == nil || time.Since(v.at) > validReuse {
		return nil, nil, false
	}
	return cl, v.tbl, true
}

// stopWait bounds how long stop and Connect wait for a connect loop to tear down.
const stopWait = 15 * time.Second

// stop cancels the connect loop and waits for its teardown; every concurrent caller waits for the same loop.
// It reports whether this call cancelled a running loop.
func (s *session) stop() bool {
	s.mu.Lock()
	cancel, done := s.runCancel, s.runDone
	s.runCancel = nil
	s.mu.Unlock()
	if done == nil {
		return false
	}
	if cancel != nil {
		cancel()
	}
	select {
	case <-done:
	case <-time.After(stopWait):
	}
	return cancel != nil
}

// Disconnect stops the connections and reports it.
func (s *session) Disconnect() {
	if s.stop() {
		s.emitConn(api.ConnDisconnected, "requested", "")
	}
}

// Close releases everything without deleting data; safe to call more than once.
func (s *session) Close() {
	s.closed.Store(true)
	s.stop()
	s.e2mu.Lock()
	st := s.st
	s.st = nil
	s.e2mu.Unlock()
	if st != nil {
		_ = st.Close()
	}
}

// Logout closes the session and deletes the E2EE device, caches and thread map.
func (s *session) Logout(ctx context.Context) error {
	s.Close()
	s.mu.Lock()
	s.wiped = true
	s.e2eeRegistered = false
	s.mu.Unlock()
	err := s.f.Wipe(s.dataDir)
	s.reg.forget()
	s.hist.reset()
	s.watch.reset(s.State())
	if err != nil {
		return api.Errorf(api.CodeIO, "failed to delete local session data")
	}
	return nil
}

// blockedReason is the fatal account signal that stopped sending, or "".
func (s *session) blockedReason() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.blocked
}

// guard runs fn; a panic is contained, logged without its value and reported as an error event.
func (s *session) guard(what string, fn func()) (panicked bool) {
	defer func() {
		if r := recover(); r != nil {
			panicked = true
			s.log.Error().Str("op", what).Msg("panic contained")
			s.emit(api.EvError, api.ErrorEvent{Code: api.CodeInternal, Message: "an internal error was contained"})
		}
	}()
	fn()
	return false
}

func (m *msgCache) reset() {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.threads = map[string]*threadCache{}
	m.index = map[string]string{}
}

// threadInfo returns a copy of the registry record for the next worker's actions.
func (s *session) threadInfo(id string) (threadEntry, bool) { return s.reg.entry(id) }
