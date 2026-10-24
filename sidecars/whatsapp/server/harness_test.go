package server

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"sync"
	"testing"
	"time"

	"github.com/rs/zerolog"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/ipc"
	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/wa"
)

const waitFor = 5 * time.Second

type frame struct {
	seq   int
	event string
	data  json.RawMessage
	reply *ipc.Reply
	used  bool
}

// harness runs the server over in-memory pipes against a fake factory.
type harness struct {
	t      *testing.T
	in     *io.PipeWriter
	done   chan error
	f      *fakeFactory
	srv    *server
	dir    string
	nextID int

	mu     sync.Mutex
	frames []*frame
}

type opt func(*server)

// startServer runs the server; the first client is a fresh unpaired one unless the factory queue holds another.
func startServer(t *testing.T, f *fakeFactory, opts ...opt) *harness {
	t.Helper()
	return startServerOpen(t, f, func(context.Context, string, zerolog.Logger) (wa.Factory, error) { return f, nil }, opts...)
}

// startServerOpen is startServer with a custom OpenFunc.
func startServerOpen(t *testing.T, f *fakeFactory, open OpenFunc, opts ...opt) *harness {
	t.Helper()
	inR, inW := io.Pipe()
	outR, outW := io.Pipe()
	h := &harness{t: t, in: inW, done: make(chan error, 1), f: f, dir: t.TempDir()}
	ready := make(chan struct{})
	go func() {
		err := run(context.Background(), inR, outW, zerolog.Nop(), open, func(s *server) {
			h.srv = s
			for _, o := range opts {
				o(s)
			}
			close(ready)
		})
		_ = outW.Close()
		h.done <- err
	}()
	go func() {
		br := bufio.NewReader(outR)
		for seq := 0; ; seq++ {
			b, err := br.ReadBytes('\n')
			if err != nil {
				return
			}
			fr := &frame{seq: seq}
			var probe map[string]json.RawMessage
			if json.Unmarshal(b, &probe) != nil {
				continue
			}
			if _, isEvent := probe["event"]; isEvent {
				var ev ipc.Event
				_ = json.Unmarshal(b, &ev)
				fr.event, fr.data = ev.Event, ev.Data
			} else {
				var rep ipc.Reply
				_ = json.Unmarshal(b, &rep)
				fr.reply = &rep
			}
			h.mu.Lock()
			h.frames = append(h.frames, fr)
			h.mu.Unlock()
		}
	}()
	<-ready
	t.Cleanup(func() { _ = inW.Close() })
	return h
}

// take blocks until an unused frame matches, marks it used and returns it.
func (h *harness) take(what string, match func(*frame) bool) *frame {
	h.t.Helper()
	deadline := time.Now().Add(waitFor)
	for time.Now().Before(deadline) {
		h.mu.Lock()
		for _, fr := range h.frames {
			if !fr.used && match(fr) {
				fr.used = true
				h.mu.Unlock()
				return fr
			}
		}
		h.mu.Unlock()
		time.Sleep(2 * time.Millisecond)
	}
	h.t.Fatalf("timed out waiting for %s", what)
	return nil
}

// event waits for the next unused event with this name and returns its data.
func (h *harness) event(name string) map[string]any {
	h.t.Helper()
	fr := h.take("event "+name, func(f *frame) bool { return f.event == name })
	var m map[string]any
	if err := json.Unmarshal(fr.data, &m); err != nil {
		h.t.Fatalf("event %s: %v", name, err)
	}
	return m
}

// eventSeq is event plus the frame sequence number, for ordering assertions.
func (h *harness) eventSeq(name string) (map[string]any, int) {
	h.t.Helper()
	fr := h.take("event "+name, func(f *frame) bool { return f.event == name })
	var m map[string]any
	_ = json.Unmarshal(fr.data, &m)
	return m, fr.seq
}

// pending lists unused events of this name after a sync round trip.
func (h *harness) pending(name string) []map[string]any {
	h.t.Helper()
	h.sync()
	h.mu.Lock()
	defer h.mu.Unlock()
	var out []map[string]any
	for _, fr := range h.frames {
		if !fr.used && fr.event == name {
			var m map[string]any
			_ = json.Unmarshal(fr.data, &m)
			out = append(out, m)
		}
	}
	return out
}

// consume marks every unused event of this name as used.
func (h *harness) consume(name string) {
	h.sync()
	h.mu.Lock()
	defer h.mu.Unlock()
	for _, fr := range h.frames {
		if fr.event == name {
			fr.used = true
		}
	}
}

// sync waits until every frame written before now has been read.
func (h *harness) sync() {
	h.t.Helper()
	h.call("ping", nil)
}

func (h *harness) send(op string, params any) int {
	h.t.Helper()
	h.nextID++
	b, err := json.Marshal(params)
	if err != nil {
		h.t.Fatal(err)
	}
	line := fmt.Sprintf(`{"id":%d,"op":%q,"params":%s}`+"\n", h.nextID, op, b)
	if _, err := io.WriteString(h.in, line); err != nil {
		h.t.Fatal(err)
	}
	return h.nextID
}

func (h *harness) reply(id int) ipc.Reply {
	h.t.Helper()
	fr := h.take(fmt.Sprintf("reply %d", id), func(f *frame) bool { return f.reply != nil && f.reply.ID == uint64(id) })
	return *fr.reply
}

func (h *harness) call(op string, params any) ipc.Reply {
	h.t.Helper()
	return h.reply(h.send(op, params))
}

// hello performs the handshake and returns the reply.
func (h *harness) hello() ipc.Reply {
	h.t.Helper()
	return h.call("hello", map[string]any{"protocol": 1, "platform": "whatsapp", "data_dir": h.dir, "state": nil})
}

func (h *harness) mustHello() map[string]any {
	h.t.Helper()
	return h.mustOK(h.hello())
}

func (h *harness) mustOK(rep ipc.Reply) map[string]any {
	h.t.Helper()
	if !rep.OK {
		h.t.Fatalf("want ok reply, got error %+v", rep.Error)
	}
	var m map[string]any
	if len(rep.Result) > 0 {
		_ = json.Unmarshal(rep.Result, &m)
	}
	return m
}

func (h *harness) wantErr(rep ipc.Reply, code string) {
	h.t.Helper()
	if rep.OK || rep.Error == nil || rep.Error.Code != code {
		h.t.Fatalf("want error %s, got %+v (ok=%v)", code, rep.Error, rep.OK)
	}
}

func (h *harness) mustCall(op string, params any) map[string]any {
	h.t.Helper()
	return h.mustOK(h.call(op, params))
}

// shutdown sends shutdown and requires Run to return cleanly.
func (h *harness) shutdown() {
	h.t.Helper()
	h.mustOK(h.call("shutdown", nil))
	select {
	case err := <-h.done:
		if err != nil {
			h.t.Fatalf("Run returned %v", err)
		}
	case <-time.After(waitFor):
		h.t.Fatal("Run did not return after shutdown")
	}
}

// pairedFactory returns a factory whose first client is linked, and the client.
func pairedFactory(connected bool) (*fakeFactory, *fakeClient) {
	c := newPairedClient(connected)
	return &fakeFactory{queue: []*fakeClient{c}}, c
}

// startPaired starts a server with a linked client and completes hello.
func startPaired(t *testing.T, connected bool, opts ...opt) (*harness, *fakeClient) {
	t.Helper()
	f, c := pairedFactory(connected)
	h := startServer(t, f, opts...)
	h.mustHello()
	return h, c
}

// fastTiming shortens every wait for tests.
func fastTiming(s *server) {
	s.tm = timing{
		history: 150 * time.Millisecond, mediaRetry: 150 * time.Millisecond, setup: 150 * time.Millisecond,
		pairCode: 150 * time.Millisecond, groupGap: 0, groupTTL: time.Hour, groupRetry: time.Hour,
	}
}

func withTiming(fn func(*timing)) opt {
	return func(s *server) {
		fastTiming(s)
		fn(&s.tm)
	}
}

// msgEvent builds a live message event.
func msgEvent(chat, sender string, fromMe bool, id string, ts int64, m *waE2E.Message) *events.Message {
	return &events.Message{
		Info: types.MessageInfo{
			MessageSource: types.MessageSource{Chat: mustJID(chat), Sender: mustJID(sender), IsFromMe: fromMe, IsGroup: mustJID(chat).Server == types.GroupServer},
			ID:            id,
			PushName:      "Alice",
			Timestamp:     time.Unix(ts, 0),
		},
		Message:    m,
		RawMessage: m,
	}
}

func text(s string) *waE2E.Message { return &waE2E.Message{Conversation: ptr(s)} }

// incoming delivers an incoming DM text and requires the handler to accept it.
func (h *harness) incoming(c *fakeClient, chat, id string, ts int64, body string) {
	h.t.Helper()
	if !c.deliver(msgEvent(chat, chat, false, id, ts, text(body))) {
		h.t.Fatal("handler refused an incoming message")
	}
}

// eventually polls cond until it holds or the wait runs out.
func eventually(t *testing.T, what string, cond func() bool) {
	t.Helper()
	deadline := time.Now().Add(waitFor)
	for time.Now().Before(deadline) {
		if cond() {
			return
		}
		time.Sleep(2 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s", what)
}
