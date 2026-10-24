package engine

import (
	"context"
	"errors"
	"sync"
	"testing"
	"time"

	"github.com/rs/zerolog"
	"go.mau.fi/mautrix-meta/pkg/messagix"
	"go.mau.fi/mautrix-meta/pkg/messagix/cookies"
	"go.mau.fi/mautrix-meta/pkg/messagix/httpclient"
	"go.mau.fi/mautrix-meta/pkg/messagix/socket"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
	"go.mau.fi/mautrix-meta/pkg/messagix/types"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/store"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

const (
	testMe   = int64(111)
	testName = "Mat Test"
)

type fakeUser struct {
	id   int64
	name string
}

func (u fakeUser) GetUsername() string  { return "" }
func (u fakeUser) GetName() string      { return u.name }
func (u fakeUser) GetAvatarURL() string { return "" }
func (u fakeUser) GetFBID() int64       { return u.id }

var _ types.UserInfo = fakeUser{}

// fakeClient is a metaClient with no network.
type fakeClient struct {
	mu          sync.Mutex
	jar         *cookies.Cookies
	user        types.UserInfo
	page        *table.LSTable
	loadErr     error
	loads       int
	handler     messagix.EventHandler
	connected   bool
	connectErr  error
	connects    int
	disconnects int
	tasks       []socket.Task
	exec        func(ctx context.Context, tasks ...socket.Task) (*table.LSTable, error)
	fetchMore   func(ctx context.Context, group int64) (*socket.KeyStoreData, *table.LSTable, error)
	posted      []*table.LSTable
}

func newFakeClient(jar *cookies.Cookies) *fakeClient {
	return &fakeClient{jar: jar, user: fakeUser{id: testMe, name: testName}, page: &table.LSTable{}}
}

func (f *fakeClient) LoadMessagesPage(context.Context) (types.UserInfo, *table.LSTable, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.loads++
	if f.loadErr != nil {
		return nil, nil, f.loadErr
	}
	return f.user, f.page, nil
}

func (f *fakeClient) Connect(context.Context) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.connects++
	if f.connectErr != nil {
		return f.connectErr
	}
	f.connected = true
	return nil
}

func (f *fakeClient) Disconnect() {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.disconnects++
	f.connected = false
}

func (f *fakeClient) IsConnected() bool {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.connected
}

func (f *fakeClient) SetEventHandler(h messagix.EventHandler) {
	f.mu.Lock()
	f.handler = h
	f.mu.Unlock()
}

func (f *fakeClient) fire(evt any) {
	f.mu.Lock()
	h := f.handler
	f.mu.Unlock()
	if h != nil {
		h(context.Background(), evt)
	}
}

func (f *fakeClient) ExecuteTasks(ctx context.Context, tasks ...socket.Task) (*table.LSTable, error) {
	f.mu.Lock()
	f.tasks = append(f.tasks, tasks...)
	exec := f.exec
	f.mu.Unlock()
	if exec == nil {
		return nil, errors.New("no exec")
	}
	return exec(ctx, tasks...)
}

func (f *fakeClient) ExecuteStatelessTask(context.Context, socket.Task) error { return nil }

func (f *fakeClient) FetchMoreThreads(ctx context.Context, g int64) (*socket.KeyStoreData, *table.LSTable, error) {
	f.mu.Lock()
	fm := f.fetchMore
	f.mu.Unlock()
	if fm == nil {
		return nil, nil, nil
	}
	return fm(ctx, g)
}

func (f *fakeClient) WaitUntilCanSendMessages(context.Context, time.Duration) error { return nil }

func (f *fakeClient) PostHandlePublishResponse(t *table.LSTable) {
	f.mu.Lock()
	f.posted = append(f.posted, t)
	f.mu.Unlock()
}

func (f *fakeClient) GetCookies() *cookies.Cookies                    { return f.jar }
func (f *fakeClient) GetCursor(int64) string                          { return "cursor" }
func (f *fakeClient) GetSyncGroupKeyStore(int64) *socket.KeyStoreData { return nil }
func (f *fakeClient) SetDevice(*store.Device)                         {}
func (f *fakeClient) RegisterE2EE(context.Context, int64) error       { return errors.New("no network") }
func (f *fakeClient) PrepareE2EEClient() (*whatsmeow.Client, error) {
	return nil, errors.New("no network")
}
func (f *fakeClient) GetHTTP() *httpclient.HTTPClient { return nil }
func (f *fakeClient) counts() (loads, connects, disconnects int) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.loads, f.connects, f.disconnects
}

type recordedEvent struct {
	name string
	data any
}

type recorder struct {
	mu     sync.Mutex
	events []recordedEvent
}

func (r *recorder) sink(name string, data any) {
	r.mu.Lock()
	r.events = append(r.events, recordedEvent{name, data})
	r.mu.Unlock()
}

func (r *recorder) all() []recordedEvent {
	r.mu.Lock()
	defer r.mu.Unlock()
	return append([]recordedEvent(nil), r.events...)
}

func (r *recorder) named(name string) []recordedEvent {
	var out []recordedEvent
	for _, e := range r.all() {
		if e.name == name {
			out = append(out, e)
		}
	}
	return out
}

// wait polls until pred holds over the recorded events.
func (r *recorder) wait(t *testing.T, what string, pred func([]recordedEvent) bool) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		if pred(r.all()) {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s; events: %v", what, r.names())
}

func (r *recorder) names() []string {
	var out []string
	for _, e := range r.all() {
		n := e.name
		if c, ok := e.data.(api.ConnectionEvent); ok {
			n += ":" + c.State + ":" + c.Reason
		}
		out = append(out, n)
	}
	return out
}

func testState() api.State {
	return api.State{
		Cookies: map[string]string{"c_user": "111", "xs": "xs-secret", "datr": "datr-secret"},
		FBID:    "111",
	}
}

func openTest(t *testing.T, fc *fakeClient) (*session, *recorder) {
	t.Helper()
	rec := &recorder{}
	f := &factory{log: zerolog.Nop(), newClient: func(c *cookies.Cookies, _ zerolog.Logger) metaClient {
		if fc.jar == nil {
			fc.jar = c
		}
		return fc
	}}
	sess, err := f.Open(testState(), t.TempDir(), rec.sink)
	if err != nil {
		t.Fatal(err)
	}
	s := sess.(*session)
	t.Cleanup(s.Close)
	return s, rec
}
