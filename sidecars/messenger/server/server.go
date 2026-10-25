// Package server runs the sidecar request loop over the ipc framing.
package server

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"runtime"
	"runtime/debug"
	"sync"
	"time"

	"github.com/rs/zerolog"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
	"github.com/matpb/meta-mcp-rs/sidecars/messenger/ipc"
)

const (
	protocolVersion = 1
	sidecarVersion  = "0.1.0"
	libraryModule   = "go.mau.fi/mautrix-meta"
	eofDrain        = 40 * time.Second
	shutdownDrain   = 5 * time.Second
	dropWait        = 20 * time.Second
)

// ErrFrame marks a protocol violation by the writer, which main turns into exit status 2.
var ErrFrame = errors.New("server: bad frame")

type line struct {
	data []byte
	err  error
}

// Login states; logged_in is implied by a non-nil session.
const (
	loginNone = iota
	loginAwaiting
	loginFinishing
)

type server struct {
	ctx context.Context
	w   *ipc.Writer
	log zerolog.Logger
	f   api.Factory
	wg  sync.WaitGroup

	hooks testHooks

	// mu guards the fields below and is never held while calling a session.
	mu          sync.Mutex
	initialised bool
	dataDir     string
	lock        *dirLock
	sess        api.Session
	login       int
	loginGen    uint64
	loginCancel context.CancelFunc
	loggingOut  int
	dropping    map[chan struct{}]struct{}
}

// Run emits the ready event, then serves requests from in until shutdown, EOF or ctx ends.
func Run(ctx context.Context, in io.Reader, out io.Writer, log zerolog.Logger, f api.Factory) error {
	return run(ctx, in, out, log, f, nil)
}

// testHooks lets tests register extra ops and pause the login flow; production passes nil.
type testHooks struct {
	extra         map[string]handler
	afterValidate func()
}

func run(ctx context.Context, in io.Reader, out io.Writer, log zerolog.Logger, f api.Factory, hooks *testHooks) error {
	ctx, cancel := context.WithCancel(ctx)
	s := &server{ctx: ctx, w: ipc.NewWriter(out), log: log, f: f}
	if hooks != nil {
		s.hooks = *hooks
	}
	defer func() {
		cancel()
		s.wait(shutdownDrain)
		s.closeSession()
		s.releaseLock()
	}()

	if err := s.w.Write(event("ready", map[string]any{"pid": os.Getpid(), "protocol": protocolVersion})); err != nil {
		return err
	}
	log.Info().Msg("sidecar ready")

	lines := make(chan line)
	done := make(chan struct{})
	defer close(done)
	go func() {
		r := ipc.NewReader(in)
		for {
			b, err := r.Next()
			select {
			case lines <- line{b, err}:
			case <-done:
				return
			}
			if err != nil {
				return
			}
		}
	}()

	for {
		var l line
		select {
		case <-ctx.Done():
			return ctx.Err()
		case l = <-lines:
		}
		if l.err != nil {
			if errors.Is(l.err, io.EOF) {
				log.Info().Msg("input closed")
				s.wait(eofDrain)
				return nil
			}
			if errors.Is(l.err, ipc.ErrTooLong) {
				log.Error().Msg("frame exceeds size limit")
				return errors.Join(ErrFrame, l.err)
			}
			log.Error().Msg("read failed")
			return l.err
		}
		req, err := ipc.DecodeRequest(l.data)
		if err != nil {
			log.Error().Msg("malformed frame")
			return errors.Join(ErrFrame, err)
		}
		if stop, err := s.handle(req); err != nil {
			return err
		} else if stop {
			log.Info().Msg("shutdown")
			return nil
		}
	}
}

// handle routes one request. hello, shutdown and the login steps run inline, the rest asynchronously.
// Before hello only ping and shutdown are answered; anything else is not_initialised.
func (s *server) handle(req ipc.Request) (stop bool, err error) {
	switch req.Op {
	case "ping":
		return false, s.reply(req.Op, ok(req.ID, map[string]any{"pong": true}))
	case "shutdown":
		return true, s.reply(req.Op, ok(req.ID, struct{}{}))
	case "hello":
		return false, s.reply(req.Op, s.inline(req, func() ipc.Reply { return s.hello(req) }))
	}
	if !s.isInitialised() {
		return false, s.reply(req.Op, fail(req.ID, api.CodeNotInitialised, "hello must be the first request"))
	}
	switch req.Op {
	case "login_start":
		return false, s.reply(req.Op, s.inline(req, func() ipc.Reply { return s.loginStart(req) }))
	case "login_cancel":
		return false, s.reply(req.Op, s.inline(req, func() ipc.Reply { return s.loginCancelOp(req) }))
	case "login_finish":
		var work func() ipc.Reply
		rep := s.inline(req, func() (r ipc.Reply) {
			r, work = s.loginFinish(req)
			return r
		})
		if work == nil {
			return false, s.reply(req.Op, rep)
		}
		s.spawn(req, func() ipc.Reply { return work() })
		return false, nil
	}
	h := s.asyncHandler(req.Op)
	if h == nil {
		return false, s.reply(req.Op, fail(req.ID, api.CodeUnsupported, "no such op"))
	}
	s.spawn(req, func() ipc.Reply { return h(s.ctx, req) })
	return false, nil
}

// inline runs an op on the reader goroutine; a panic becomes an internal error reply.
func (s *server) inline(req ipc.Request, fn func() ipc.Reply) (rep ipc.Reply) {
	defer func() {
		if recover() != nil {
			s.log.Error().Str("op", req.Op).Msg("handler panicked")
			rep = fail(req.ID, api.CodeInternal, "internal error")
		}
	}()
	return fn()
}

// spawn runs fn in its own goroutine and writes its reply; a panic becomes an internal error reply.
func (s *server) spawn(req ipc.Request, fn func() ipc.Reply) {
	s.wg.Add(1)
	go func() {
		defer s.wg.Done()
		rep := func() (rep ipc.Reply) {
			defer func() {
				if recover() != nil {
					s.log.Error().Str("op", req.Op).Msg("handler panicked")
					rep = fail(req.ID, api.CodeInternal, "internal error")
				}
			}()
			return fn()
		}()
		_ = s.reply(req.Op, rep)
	}()
}

// wait blocks until in-flight handlers finish or grace passes.
func (s *server) wait(grace time.Duration) {
	finished := make(chan struct{})
	go func() {
		s.wg.Wait()
		close(finished)
	}()
	select {
	case <-finished:
	case <-time.After(grace):
	}
}

func (s *server) isInitialised() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.initialised
}

func (s *server) reply(op string, rep ipc.Reply) error {
	if rep.OK {
		s.log.Info().Str("op", op).Str("code", "ok").Msg("request done")
	} else if rep.Error != nil {
		s.log.Info().Str("op", op).Str("code", rep.Error.Code).Msg("request done")
	}
	err := s.w.Write(rep)
	if errors.Is(err, ipc.ErrTooLong) {
		s.log.Error().Str("op", op).Msg("reply too large")
		return s.w.Write(fail(rep.ID, api.CodeInternal, "the reply exceeds the 16 MiB line limit"))
	}
	return err
}

func (s *server) emit(name string, data any) {
	if err := s.w.Write(event(name, data)); err != nil {
		s.log.Error().Str("event", name).Msg("event write failed")
	}
}

// releaseLock drops the data_dir lock at shutdown.
func (s *server) releaseLock() {
	s.mu.Lock()
	l := s.lock
	s.lock = nil
	s.mu.Unlock()
	l.release()
}

// closeSession disconnects and closes the current session, if any.
func (s *server) closeSession() {
	s.mu.Lock()
	sess := s.sess
	s.sess = nil
	s.mu.Unlock()
	if sess != nil {
		sess.Disconnect()
		sess.Close()
	}
}

// session returns the current session or nil.
func (s *server) session() api.Session {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.sess
}

// sinkRef ties a sink to the session it was created for, which is known only after install.
type sinkRef struct {
	mu   sync.Mutex
	sess api.Session
	lost bool
}

func (s *server) newSink() (api.Sink, *sinkRef) {
	ref := &sinkRef{}
	return func(name string, data any) {
		if name == api.EvLoggedOut {
			ref.mu.Lock()
			sess := ref.sess
			if sess == nil {
				ref.lost = true
			}
			ref.mu.Unlock()
			if sess != nil {
				s.dropIf(sess)
			}
		}
		s.emit(name, data)
	}, ref
}

var (
	errLoginCancelled = errors.New("login cancelled")
	errSessionLost    = errors.New("session ended")
)

// install makes sess current. With gen non-nil the login generation is checked in the same
// critical section as the assignment, so a cancel or logout can never be overtaken.
func (s *server) install(sess api.Session, ref *sinkRef, gen *uint64) error {
	s.mu.Lock()
	if gen != nil && s.loginGen != *gen {
		s.mu.Unlock()
		return errLoginCancelled
	}
	s.sess = sess
	s.login = loginNone
	s.loginCancel = nil
	s.mu.Unlock()
	ref.mu.Lock()
	ref.sess = sess
	lost := ref.lost
	ref.mu.Unlock()
	if lost {
		s.dropIf(sess)
		return errSessionLost
	}
	return nil
}

// dropIf forgets sess when it is still current and closes it in the background; Close goes first so a
// dying session writes nothing more to the data dir, and logout waits for it (see waitDropped).
func (s *server) dropIf(sess api.Session) {
	s.mu.Lock()
	if s.sess != sess {
		s.mu.Unlock()
		return
	}
	s.sess = nil
	s.login = loginNone
	done := make(chan struct{})
	if s.dropping == nil {
		s.dropping = map[chan struct{}]struct{}{}
	}
	s.dropping[done] = struct{}{}
	s.mu.Unlock()
	go func() {
		defer func() {
			if recover() != nil {
				s.log.Error().Str("op", "drop").Msg("handler panicked")
			}
			s.mu.Lock()
			delete(s.dropping, done)
			s.mu.Unlock()
			close(done)
		}()
		sess.Close()
		sess.Disconnect()
	}()
}

// waitDropped blocks until the sessions that were dropped in the background are closed, for at most dropWait.
func (s *server) waitDropped() {
	s.mu.Lock()
	pending := make([]chan struct{}, 0, len(s.dropping))
	for ch := range s.dropping {
		pending = append(pending, ch)
	}
	s.mu.Unlock()
	timeout := time.After(dropWait)
	for _, ch := range pending {
		select {
		case <-ch:
		case <-timeout:
			return
		}
	}
}

type helloParams struct {
	Protocol int             `json:"protocol"`
	Platform string          `json:"platform"`
	DataDir  string          `json:"data_dir"`
	State    json.RawMessage `json:"state"`
}

func (s *server) hello(req ipc.Request) ipc.Reply {
	if s.isInitialised() {
		return fail(req.ID, api.CodeBadParams, "already initialised")
	}
	var p helloParams
	if isNull(req.Params) || json.Unmarshal(req.Params, &p) != nil {
		return fail(req.ID, api.CodeBadParams, "hello params must be an object")
	}
	if p.Protocol != protocolVersion {
		return fail(req.ID, api.CodeProtocolMismatch, "this sidecar speaks protocol 1")
	}
	if p.Platform != "messenger" {
		return fail(req.ID, api.CodeBadParams, "platform must be messenger")
	}
	if msg := checkDataDir(p.DataDir, s.log); msg != "" {
		return fail(req.ID, api.CodeBadParams, msg)
	}
	var st *api.State
	if !isNull(p.State) {
		st = &api.State{}
		if json.Unmarshal(p.State, st) != nil || !stateValid(st) {
			return fail(req.ID, api.CodeBadParams, "state is invalid")
		}
	}
	lock, err := lockDataDir(p.DataDir)
	if err != nil {
		if errors.Is(err, errDirLocked) {
			return fail(req.ID, api.CodeIO, "another sidecar process holds this data_dir")
		}
		return fail(req.ID, api.CodeIO, "could not lock data_dir")
	}
	committed := false
	defer func() {
		if !committed {
			lock.release()
		}
	}()
	var acct any
	if st != nil {
		sink, ref := s.newSink()
		sess, err := s.f.Open(*st, p.DataDir, sink)
		if err != nil {
			return failErr(req.ID, err)
		}
		s.mu.Lock()
		s.initialised = true
		s.dataDir = p.DataDir
		s.lock = lock
		s.mu.Unlock()
		committed = true
		s.install(sess, ref, nil)
		acct = api.Account{ID: st.FBID}
	} else {
		s.mu.Lock()
		s.initialised = true
		s.dataDir = p.DataDir
		s.lock = lock
		s.mu.Unlock()
		committed = true
	}
	return ok(req.ID, map[string]any{
		"protocol":        protocolVersion,
		"sidecar_version": sidecarVersion,
		"library":         map[string]string{"name": "mautrix-meta", "version": libraryVersion()},
		"go":              runtime.Version(),
		"logged_in":       acct != nil,
		"account":         acct,
	})
}

// checkDataDir returns a bad_params message, or "" when dir is acceptable.
func checkDataDir(dir string, log zerolog.Logger) string {
	if dir == "" || !isAbsClean(dir) {
		return "data_dir must be an absolute, clean path"
	}
	fi, err := os.Stat(dir)
	if err != nil || !fi.IsDir() {
		return "data_dir must be an existing directory"
	}
	if dirModeLoose(fi) {
		log.Warn().Msg("data_dir mode is not 0700")
	}
	return ""
}

func stateValid(st *api.State) bool {
	c := st.Cookies
	return c["c_user"] != "" && c["xs"] != "" && c["datr"] != "" && st.FBID == c["c_user"]
}

// libraryVersion reads the mautrix-meta module version from the binary's build info.
func libraryVersion() string {
	if bi, found := debug.ReadBuildInfo(); found {
		for _, d := range bi.Deps {
			if d.Path == libraryModule {
				return d.Version
			}
		}
	}
	return "unknown"
}

func isNull(raw json.RawMessage) bool {
	return len(raw) == 0 || string(raw) == "null"
}

func ok(id uint64, result any) ipc.Reply {
	b, err := json.Marshal(result)
	if err != nil {
		return fail(id, api.CodeInternal, "result encoding failed")
	}
	return ipc.Reply{ID: id, OK: true, Result: b}
}

func fail(id uint64, code, msg string) ipc.Reply {
	return ipc.Reply{ID: id, Error: &ipc.ReplyError{Code: code, Message: msg}}
}

func failErr(id uint64, err error) ipc.Reply {
	e := api.AsError(err)
	return fail(id, e.Code, e.Msg)
}

func event(name string, data any) ipc.Event {
	b, _ := json.Marshal(data)
	return ipc.Event{Event: name, Data: b}
}
