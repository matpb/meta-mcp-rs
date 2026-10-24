// Package server runs the sidecar request loop over the ipc framing.
package server

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"runtime/debug"
	"sync"
	"sync/atomic"
	"time"

	"github.com/rs/zerolog"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/ipc"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/store"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/wa"
)

const (
	protocolVersion = 1
	sidecarVersion  = "0.1.0"
	libraryModule   = "go.mau.fi/whatsmeow"
	drainGrace      = 5 * time.Second
	messagesDBName  = "messages.db"
)

// ErrFrame marks a protocol violation by the writer, which main turns into exit status 2.
var ErrFrame = errors.New("server: bad frame")

// OpenFunc opens the session factory (session.db and the data_dir lock) for a data_dir.
type OpenFunc func(ctx context.Context, dataDir string, log zerolog.Logger) (wa.Factory, error)

// handler serves one request; it may block and runs in its own goroutine.
type handler func(ctx context.Context, req ipc.Request) ipc.Reply

type line struct {
	data []byte
	err  error
}

// timing holds the waits the tests shorten.
type timing struct {
	history    time.Duration // on-demand history answer
	mediaRetry time.Duration // media retry answer
	setup      time.Duration // Connected and prekeys after pairing
	pairCode   time.Duration // first QR item before PairPhone
	groupGap   time.Duration // minimum gap between group info fetches
	groupTTL   time.Duration // group info older than this is refreshed
	groupRetry time.Duration // wait before retrying a failed group info fetch
	unlink     time.Duration // longest wait for whatsmeow to delete the device after LoggedOut
}

func defaultTiming() timing {
	return timing{
		history:    30 * time.Second,
		mediaRetry: 20 * time.Second,
		setup:      120 * time.Second,
		pairCode:   30 * time.Second,
		groupGap:   time.Second,
		groupTTL:   24 * time.Hour,
		groupRetry: 10 * time.Minute,
		unlink:     5 * time.Second,
	}
}

// Run emits the ready event, then serves requests from in until shutdown, EOF or ctx ends.
func Run(ctx context.Context, in io.Reader, out io.Writer, log zerolog.Logger, open OpenFunc) error {
	return run(ctx, in, out, log, open, nil)
}

// run is Run with a hook that tests use to shorten the waits.
func run(ctx context.Context, in io.Reader, out io.Writer, log zerolog.Logger, open OpenFunc, tweak func(*server)) error {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	s := newServer(ctx, ipc.NewWriter(out), log, open)
	if tweak != nil {
		tweak(s)
	}
	defer s.cleanup()

	if err := s.w.Write(event(api.EvReady, map[string]any{"pid": os.Getpid(), "protocol": protocolVersion})); err != nil {
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
				s.drain()
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

// server holds the session state shared by the request goroutines and the event handler.
type server struct {
	ctx    context.Context // cancelled when the process is shutting down
	cancel context.CancelFunc
	w      *ipc.Writer
	log    zerolog.Logger
	open   OpenFunc
	ops    map[string]handler
	wg     sync.WaitGroup // in-flight request goroutines
	bg     sync.WaitGroup // background goroutines
	tm     timing
	closed atomic.Bool

	mu          sync.Mutex
	initialised bool
	dataDir     string
	factory     wa.Factory
	store       *store.Store
	client      wa.Client
	blocked     string // fatal reason that refuses sends until restart
	wantConn    bool   // the daemon asked for a connection, so a drop is reported
	login       *login

	loginMu  sync.Mutex // serialises login_start
	ingestMu sync.Mutex // serialises store writes of the event handler

	hist   histWaiters
	media  mediaWaiters
	groups groupFetcher
}

func newServer(ctx context.Context, w *ipc.Writer, log zerolog.Logger, open OpenFunc) *server {
	opCtx, cancel := context.WithCancel(ctx)
	s := &server{ctx: opCtx, cancel: cancel, w: w, log: log, open: open, tm: defaultTiming()}
	s.hist = histWaiters{m: map[string]*histWait{}}
	s.media = mediaWaiters{m: map[string][]chan *mediaRetryResult{}}
	s.groups = groupFetcher{tried: map[string]time.Time{}}
	s.ops = map[string]handler{
		"ping": func(_ context.Context, req ipc.Request) ipc.Reply { return ok(req.ID, map[string]any{"pong": true}) },

		"login_start":  s.loginStart,
		"login_finish": s.loginFinish,
		"login_cancel": s.loginCancel,
		"logout":       s.logout,
		"connect":      s.connect,
		"disconnect":   s.disconnect,

		"list_threads":        s.listThreads,
		"fetch_messages":      s.fetchMessages,
		"download_attachment": s.downloadAttachment,

		"send_text":     s.sendText,
		"send_media":    s.sendMedia,
		"send_reaction": s.sendReaction,
		"mark_read":     s.markRead,
		"set_typing":    s.setTyping,
	}
	return s
}

func (s *server) isInit() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.initialised
}

// handle routes one request. hello and shutdown run inline, everything else asynchronously.
// Before hello only ping, hello and shutdown are served; any other op is not_initialised.
func (s *server) handle(req ipc.Request) (stop bool, err error) {
	if req.Op != "hello" && req.Op != "shutdown" && req.Op != "ping" && !s.isInit() {
		return false, s.reply(req.Op, fail(req.ID, api.CodeNotInitialised, "hello must be the first request"))
	}
	switch req.Op {
	case "hello":
		rep, post := s.safeHello(req)
		err := s.reply(req.Op, rep)
		if err == nil && post != nil {
			post()
		}
		return false, err
	case "shutdown":
		s.cancel()
		s.drain()
		return true, s.reply(req.Op, ok(req.ID, struct{}{}))
	}
	h, found := s.ops[req.Op]
	if !found {
		return false, s.reply(req.Op, fail(req.ID, api.CodeUnsupported, "no such op"))
	}
	s.dispatchAsync(req, h)
	return false, nil
}

// dispatchAsync runs h in its own goroutine and writes its reply when it returns.
func (s *server) dispatchAsync(req ipc.Request, h handler) {
	s.wg.Add(1)
	go func() {
		defer s.wg.Done()
		_ = s.reply(req.Op, s.safely(req, h))
	}()
}

// safely runs h and turns a panic into an internal error reply.
func (s *server) safely(req ipc.Request, h handler) (rep ipc.Reply) {
	defer func() {
		if r := recover(); r != nil {
			s.recovered("op", req.Op, r)
			rep = fail(req.ID, api.CodeInternal, "internal error")
		}
	}()
	return h(s.ctx, req)
}

// safeHello is hello with the same panic guard as the async ops.
func (s *server) safeHello(req ipc.Request) (rep ipc.Reply, post func()) {
	defer func() {
		if r := recover(); r != nil {
			s.recovered("op", req.Op, r)
			rep, post = fail(req.ID, api.CodeInternal, "internal error"), nil
		}
	}()
	return s.hello(req)
}

// recovered reports a recovered panic by its type only, since the panic value could carry message text.
// kind is "op", "event" or "background"; name is the op, the event type or the goroutine.
func (s *server) recovered(kind, name string, r any) {
	ev := s.log.Error().Str("reason", fmt.Sprintf("%T", r))
	switch kind {
	case "op":
		ev = ev.Str("component", kind).Str("op", name)
	case "event":
		ev = ev.Str("component", kind).Str("event", name)
	default:
		ev = ev.Str("component", kind+"/"+name)
	}
	ev.Msg("recovered a panic")
	s.emitLog(api.EvError, api.ErrorEvent{Code: api.CodeInternal, Message: "an internal error was recovered in " + kind + " code"})
}

// recoverPanic is deferred at the top of a goroutine that nothing else guards.
func (s *server) recoverPanic(name string) {
	if r := recover(); r != nil {
		s.recovered("background", name, r)
	}
}

// drain waits a bounded time for in-flight handlers to write their replies.
func (s *server) drain() {
	waitTimeout(&s.wg, drainGrace)
}

func waitTimeout(wg *sync.WaitGroup, d time.Duration) {
	finished := make(chan struct{})
	go func() {
		wg.Wait()
		close(finished)
	}()
	select {
	case <-finished:
	case <-time.After(d):
	}
}

// cleanup stops background work, disconnects and closes the stores; it runs once when Run returns.
func (s *server) cleanup() {
	s.closed.Store(true)
	s.cancel()
	waitTimeout(&s.bg, drainGrace)
	s.mu.Lock()
	cl, st, f := s.client, s.store, s.factory
	s.wantConn = false
	if s.login != nil {
		s.login.cancel()
	}
	s.mu.Unlock()
	if cl != nil {
		cl.Disconnect()
	}
	if st != nil {
		_ = st.Close()
	}
	if f != nil {
		_ = f.Close()
	}
}

func (s *server) reply(op string, rep ipc.Reply) error {
	if rep.OK {
		s.log.Info().Str("op", op).Msg("request ok")
	} else if rep.Error != nil {
		s.log.Info().Str("op", op).Str("code", rep.Error.Code).Msg("request failed")
	}
	err := s.w.Write(rep)
	if errors.Is(err, ipc.ErrTooLong) {
		s.log.Error().Str("op", op).Msg("reply too large")
		return s.w.Write(fail(rep.ID, api.CodeInternal, "the reply exceeds the 16 MiB line limit"))
	}
	return err
}

// emit writes one event frame and reports a write failure.
func (s *server) emit(name string, data any) error {
	b, err := json.Marshal(data)
	if err != nil {
		return err
	}
	if err := s.w.Write(ipc.Event{Event: name, Data: b}); err != nil {
		if errors.Is(err, ipc.ErrTooLong) {
			s.log.Info().Str("event", name).Msg("event over the frame limit")
		} else {
			s.log.Error().Str("event", name).Msg("event write failed")
		}
		return err
	}
	return nil
}

// emitLog is emit for callers that cannot act on a write failure.
func (s *server) emitLog(name string, data any) { _ = s.emit(name, data) }

type helloParams struct {
	Protocol int             `json:"protocol"`
	Platform string          `json:"platform"`
	DataDir  string          `json:"data_dir"`
	State    json.RawMessage `json:"state"`
}

// hello validates the handshake and, on the first call, opens the session and message stores.
func (s *server) hello(req ipc.Request) (ipc.Reply, func()) {
	if s.isInit() {
		return fail(req.ID, api.CodeBadParams, "already initialised"), nil
	}
	var p helloParams
	if !isObject(req.Params) || json.Unmarshal(req.Params, &p) != nil {
		return fail(req.ID, api.CodeBadParams, "hello params must be an object with protocol, platform, data_dir and state"), nil
	}
	if p.Protocol != protocolVersion || p.Platform != "whatsapp" {
		return fail(req.ID, api.CodeProtocolMismatch, "this sidecar speaks protocol 1 for platform whatsapp"), nil
	}
	if !filepath.IsAbs(p.DataDir) {
		return fail(req.ID, api.CodeBadParams, "data_dir must be an absolute path"), nil
	}
	fi, err := os.Stat(p.DataDir)
	if err != nil || !fi.IsDir() {
		return fail(req.ID, api.CodeBadParams, "data_dir must be an existing directory"), nil
	}
	if fi.Mode().Perm() != 0o700 {
		s.log.Info().Msg("data_dir mode is not 0700")
	}
	hasState := len(p.State) > 0 && string(p.State) != "null"

	if rep, failed := s.firstHello(req.ID, filepath.Clean(p.DataDir)); failed {
		return rep, nil
	}

	cl := s.cur()
	acc := accountOf(cl)
	rep := ok(req.ID, map[string]any{
		"protocol":        protocolVersion,
		"sidecar_version": sidecarVersion,
		"library":         map[string]string{"name": "whatsmeow", "version": libraryVersion()},
		"go":              runtime.Version(),
		"logged_in":       cl.HasDevice(),
		"account":         acc,
	})
	if hasState && !cl.HasDevice() {
		return rep, func() { s.emitLog(api.EvStateUpdated, api.StateUpdatedEvent{}) }
	}
	return rep, nil
}

// firstHello opens the factory, messages.db and the first client; failed reports an error reply.
func (s *server) firstHello(id uint64, dir string) (rep ipc.Reply, failed bool) {
	f, err := s.open(s.ctx, dir, s.log)
	if err != nil {
		if errors.Is(err, wa.ErrLocked) {
			return fail(id, api.CodeIO, "another sidecar process holds this data_dir"), true
		}
		return fail(id, api.CodeIO, "could not open the session database"), true
	}
	st, err := store.Open(filepath.Join(dir, messagesDBName))
	if err != nil {
		_ = f.Close()
		return fail(id, api.CodeIO, "could not open the message database"), true
	}
	cl, err := f.NewClient(s.ctx, nil)
	if err != nil {
		_ = st.Close()
		_ = f.Close()
		return fail(id, api.CodeIO, "could not load the WhatsApp session"), true
	}
	s.mu.Lock()
	s.initialised, s.dataDir, s.factory, s.store = true, dir, f, st
	s.mu.Unlock()
	s.setClient(cl)
	return ipc.Reply{}, false
}

// accountOf returns the linked account or nil while unpaired.
func accountOf(cl wa.Client) *api.Account {
	if !cl.HasDevice() {
		return nil
	}
	self := cl.Self()
	if self.JID.IsEmpty() {
		return nil
	}
	return &api.Account{ID: self.JID.String(), Name: self.PushName}
}

// libraryVersion reads the whatsmeow module version from the build info.
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

func isObject(raw json.RawMessage) bool {
	for _, c := range raw {
		switch c {
		case ' ', '\t', '\r', '\n':
			continue
		}
		return c == '{'
	}
	return false
}

// decode parses req.Params into v; null or missing params decode as an empty object.
func decode(req ipc.Request, v any) *api.Error {
	raw := req.Params
	if len(raw) == 0 || string(raw) == "null" {
		raw = json.RawMessage("{}")
	}
	if !isObject(raw) || json.Unmarshal(raw, v) != nil {
		return api.Errorf(api.CodeBadParams, "params are not valid for this op")
	}
	return nil
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

// failErr maps any error to a protocol failure; only *api.Error carries its own text.
func failErr(id uint64, err error) ipc.Reply {
	e := api.AsError(err)
	return fail(id, e.Code, e.Msg)
}

func event(name string, data any) ipc.Event {
	b, _ := json.Marshal(data)
	return ipc.Event{Event: name, Data: b}
}

// cur returns the current client.
func (s *server) cur() wa.Client {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.client
}

// setClient makes cl the current client and routes its events to the handler.
func (s *server) setClient(cl wa.Client) {
	s.mu.Lock()
	s.client = cl
	s.mu.Unlock()
	cl.SetEventHandler(func(evt any) bool { return s.handleEvent(cl, evt) })
}

// freshClient disconnects the current client and installs a new unpaired or stored one.
func (s *server) freshClient(ctx context.Context) error {
	s.mu.Lock()
	old := s.client
	s.wantConn = false
	s.mu.Unlock()
	if old != nil {
		old.Disconnect()
	}
	cl, err := s.factory.NewClient(ctx, nil)
	if err != nil {
		return err
	}
	s.setClient(cl)
	return nil
}

func (s *server) setWantConn(v bool) {
	s.mu.Lock()
	s.wantConn = v
	s.mu.Unlock()
}

func (s *server) wantsConn() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.wantConn
}

// stateBlob builds the opaque state the daemon persists; nil when there is no linked device.
func (s *server) stateBlob(ctx context.Context, cl wa.Client) *api.State {
	if !cl.HasDevice() {
		return nil
	}
	self := cl.Self()
	st := &api.State{PushName: self.PushName}
	if !self.JID.IsEmpty() {
		st.JID = self.JID.String()
	}
	if !self.LID.IsEmpty() {
		st.LID = self.LID.String()
	}
	if v, found, err := s.store.GetMeta(ctx, metaPairedAt); err == nil && found {
		st.PairedAt = parseInt(v)
	}
	if v, found, err := s.store.GetMeta(ctx, metaFullHistory); err == nil && found {
		st.FullHistory = v == "true"
	}
	return st
}
