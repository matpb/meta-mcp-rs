package server

import (
	"context"
	"sync"
	"sync/atomic"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

// fakeFactory records every Open and hands out fakeSessions wired to the hooks below.
type fakeFactory struct {
	mu       sync.Mutex
	opens    []api.State
	dataDirs []string
	sessions []*fakeSession
	sink     api.Sink
	openErr  error

	validate    func(ctx context.Context) (api.Account, error)
	listThreads func(ctx context.Context, p api.ListThreadsParams) (api.ListThreadsResult, error)
	sendText    func(ctx context.Context, p api.SendTextParams) (api.SendResult, error)
}

func (f *fakeFactory) Open(st api.State, dataDir string, sink api.Sink) (api.Session, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.openErr != nil {
		return nil, f.openErr
	}
	s := &fakeSession{f: f, st: st}
	f.opens = append(f.opens, st)
	f.dataDirs = append(f.dataDirs, dataDir)
	f.sessions = append(f.sessions, s)
	f.sink = sink
	return s, nil
}

func (f *fakeFactory) lastSession() *fakeSession {
	f.mu.Lock()
	defer f.mu.Unlock()
	if len(f.sessions) == 0 {
		return nil
	}
	return f.sessions[len(f.sessions)-1]
}

func (f *fakeFactory) emit(name string, data any) {
	f.mu.Lock()
	sink := f.sink
	f.mu.Unlock()
	sink(name, data)
}

type fakeSession struct {
	f            *fakeFactory
	st           api.State
	closed       atomic.Int32
	disconnected atomic.Int32
	loggedOut    atomic.Int32
}

func (s *fakeSession) Validate(ctx context.Context) (api.Account, error) {
	if s.f.validate != nil {
		return s.f.validate(ctx)
	}
	return api.Account{ID: s.st.FBID, Name: "Test User"}, nil
}

func (s *fakeSession) Connect(context.Context) error { return nil }
func (s *fakeSession) Disconnect()                   { s.disconnected.Add(1) }
func (s *fakeSession) Close()                        { s.closed.Add(1) }
func (s *fakeSession) State() api.State              { return s.st }

func (s *fakeSession) Logout(context.Context) error {
	s.loggedOut.Add(1)
	return nil
}

func (s *fakeSession) ListThreads(ctx context.Context, p api.ListThreadsParams) (api.ListThreadsResult, error) {
	if s.f.listThreads != nil {
		return s.f.listThreads(ctx, p)
	}
	return api.ListThreadsResult{}, nil
}

func (s *fakeSession) FetchMessages(context.Context, api.FetchMessagesParams) (api.FetchMessagesResult, error) {
	return api.FetchMessagesResult{}, nil
}

func (s *fakeSession) DownloadAttachment(context.Context, api.DownloadParams) (api.DownloadResult, error) {
	return api.DownloadResult{}, nil
}

func (s *fakeSession) SendText(ctx context.Context, p api.SendTextParams) (api.SendResult, error) {
	if s.f.sendText != nil {
		return s.f.sendText(ctx, p)
	}
	return api.SendResult{MessageID: "fb:mid.1", TS: 1700000000}, nil
}

func (s *fakeSession) SendMedia(context.Context, api.SendMediaParams) (api.SendResult, error) {
	return api.SendResult{MessageID: "fb:mid.2", TS: 1700000001}, nil
}

func (s *fakeSession) SendReaction(context.Context, api.SendReactionParams) error { return nil }
func (s *fakeSession) MarkRead(context.Context, api.MarkReadParams) error         { return nil }
func (s *fakeSession) SetTyping(context.Context, api.SetTypingParams) error       { return nil }
