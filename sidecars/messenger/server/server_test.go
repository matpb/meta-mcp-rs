package server

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/rs/zerolog"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
	"github.com/matpb/meta-mcp-rs/sidecars/messenger/ipc"
)

type harness struct {
	t      *testing.T
	toSrv  *io.PipeWriter
	fromSr *bufio.Reader
	done   chan error
	f      *fakeFactory
	dir    string
	events []ipc.Event
}

func start(t *testing.T) *harness {
	t.Helper()
	return startWith(t, nil)
}

func startWith(t *testing.T, hooks *testHooks) *harness {
	t.Helper()
	inR, inW := io.Pipe()
	outR, outW := io.Pipe()
	h := &harness{t: t, toSrv: inW, fromSr: bufio.NewReader(outR), done: make(chan error, 1), f: &fakeFactory{}, dir: t.TempDir()}
	go func() {
		err := run(context.Background(), inR, outW, zerolog.Nop(), h.f, hooks)
		outW.Close()
		h.done <- err
	}()
	t.Cleanup(func() { inW.Close() })
	return h
}

func (h *harness) readLine() []byte {
	h.t.Helper()
	ch := make(chan []byte, 1)
	go func() {
		b, _ := h.fromSr.ReadBytes('\n')
		ch <- b
	}()
	select {
	case b := <-ch:
		return b
	case <-time.After(5 * time.Second):
		h.t.Fatal("timed out waiting for a frame")
		return nil
	}
}

func (h *harness) send(line string) {
	h.t.Helper()
	if _, err := io.WriteString(h.toSrv, line+"\n"); err != nil {
		h.t.Fatal(err)
	}
}

// nextReply reads frames until a reply arrives, keeping events in h.events.
func (h *harness) nextReply() ipc.Reply {
	h.t.Helper()
	for {
		b := h.readLine()
		var probe map[string]json.RawMessage
		if err := json.Unmarshal(b, &probe); err != nil {
			h.t.Fatal(err)
		}
		if _, isEvent := probe["event"]; isEvent {
			var ev ipc.Event
			if err := json.Unmarshal(b, &ev); err != nil {
				h.t.Fatal(err)
			}
			h.events = append(h.events, ev)
			continue
		}
		var rep ipc.Reply
		if err := json.Unmarshal(b, &rep); err != nil {
			h.t.Fatal(err)
		}
		return rep
	}
}

func (h *harness) call(line string) ipc.Reply {
	h.t.Helper()
	h.send(line)
	return h.nextReply()
}

func (h *harness) nextEvent() ipc.Event {
	h.t.Helper()
	var ev ipc.Event
	if err := json.Unmarshal(h.readLine(), &ev); err != nil || ev.Event == "" {
		h.t.Fatalf("want an event frame: %v", err)
	}
	return ev
}

func (h *harness) req(id int, op, params string) string {
	return fmt.Sprintf(`{"id":%d,"op":%q,"params":%s}`, id, op, params)
}

func (h *harness) helloLine(id int, state string) string {
	return h.req(id, "hello", fmt.Sprintf(`{"protocol":1,"platform":"messenger","data_dir":%q,"state":%s}`, h.dir, state))
}

const goodState = `{"cookies":{"c_user":"1000123456789012","xs":"XSVAL","datr":"DATRVAL"},"user_agent":"UA","fbid":"1000123456789012","e2ee_registered":false}`

const goodCookies = `{"c_user":"1000123456789012","xs":"XSSECRET","datr":"DATRSECRET","sb":"SBSECRET","fr":"FRSECRET"}`

func wantErr(t *testing.T, rep ipc.Reply, id uint64, code string) {
	t.Helper()
	if rep.ID != id || rep.OK || rep.Error == nil || rep.Error.Code != code {
		t.Fatalf("want error %q for id %d, got %+v (%v)", code, id, rep, rep.Error)
	}
}

func wantOK(t *testing.T, rep ipc.Reply, id uint64) map[string]any {
	t.Helper()
	if rep.ID != id || !rep.OK || rep.Error != nil {
		t.Fatalf("want ok for id %d, got %+v (%v)", id, rep, rep.Error)
	}
	var m map[string]any
	if err := json.Unmarshal(rep.Result, &m); err != nil {
		t.Fatal(err)
	}
	return m
}

func (h *harness) ready() {
	h.t.Helper()
	ev := h.nextEvent()
	var data struct{ PID, Protocol int }
	if err := json.Unmarshal(ev.Data, &data); err != nil {
		h.t.Fatal(err)
	}
	if ev.Event != "ready" || data.PID == 0 || data.Protocol != 1 {
		h.t.Fatalf("unexpected first frame %+v %+v", ev, data)
	}
}

func (h *harness) helloOK(state string) map[string]any {
	h.t.Helper()
	return wantOK(h.t, h.call(h.helloLine(1, state)), 1)
}

func (h *harness) waitDone() error {
	h.t.Helper()
	select {
	case err := <-h.done:
		return err
	case <-time.After(5 * time.Second):
		h.t.Fatal("Run did not return")
		return nil
	}
}

func TestBeforeHello(t *testing.T) {
	h := start(t)
	h.ready()
	wantOK(t, h.call(h.req(1, "ping", "null")), 1)
	for i, op := range []string{"login_start", "login_finish", "login_cancel", "logout", "connect", "list_threads", "send_text", "frobnicate"} {
		wantErr(t, h.call(h.req(10+i, op, "null")), uint64(10+i), "not_initialised")
	}
	rep := h.call(h.req(30, "shutdown", "null"))
	wantOK(t, rep, 30)
	if err := h.waitDone(); err != nil {
		t.Fatalf("Run returned %v", err)
	}
}

func TestHelloValidation(t *testing.T) {
	h := start(t)
	h.ready()
	bad := func(id int, params, code string) {
		t.Helper()
		wantErr(t, h.call(h.req(id, "hello", params)), uint64(id), code)
	}
	file := filepath.Join(h.dir, "afile")
	if err := os.WriteFile(file, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	bad(1, "null", "bad_params")
	bad(2, `{"protocol":2,"platform":"messenger","data_dir":"/tmp","state":null}`, "protocol_mismatch")
	bad(3, `{"protocol":1,"platform":"whatsapp","data_dir":"/tmp","state":null}`, "bad_params")
	bad(4, `{"protocol":1,"platform":"messenger","data_dir":"relative/dir","state":null}`, "bad_params")
	bad(5, fmt.Sprintf(`{"protocol":1,"platform":"messenger","data_dir":%q,"state":null}`, h.dir+"/../"+filepath.Base(h.dir)), "bad_params")
	bad(6, fmt.Sprintf(`{"protocol":1,"platform":"messenger","data_dir":%q,"state":null}`, h.dir+"/missing"), "bad_params")
	bad(7, fmt.Sprintf(`{"protocol":1,"platform":"messenger","data_dir":%q,"state":null}`, file), "bad_params")
	stateBad := func(id int, state string) {
		t.Helper()
		rep := h.call(h.helloLine(id, state))
		wantErr(t, rep, uint64(id), "bad_params")
		if rep.Error.Message != "state is invalid" {
			t.Fatalf("message %q", rep.Error.Message)
		}
	}
	stateBad(8, `"nope"`)
	stateBad(9, `{"cookies":{"c_user":"1","xs":"x"},"fbid":"1"}`)
	stateBad(10, `{"cookies":{"c_user":"1","xs":"x","datr":"d"},"fbid":"2"}`)
	stateBad(11, `{"cookies":{"c_user":"1","xs":"","datr":"d"},"fbid":"1"}`)
	if len(h.f.opens) != 0 {
		t.Fatal("Open must not run for a rejected hello")
	}
}

func TestHelloNoSessionAndSecondHello(t *testing.T) {
	h := start(t)
	h.ready()
	res := h.helloOK("null")
	lib, _ := res["library"].(map[string]any)
	if res["protocol"] != float64(1) || res["sidecar_version"] != "0.1.0" || res["logged_in"] != false || res["account"] != nil ||
		lib["name"] != "mautrix-meta" || lib["version"] == "" || !strings.HasPrefix(res["go"].(string), "go1.") {
		t.Fatalf("unexpected hello result %v", res)
	}
	wantErr(t, h.call(h.helloLine(2, "null")), 2, "bad_params")
	rep := h.call(h.req(3, "bogus", "null"))
	wantErr(t, rep, 3, "unsupported")
	if rep.Error.Message != "no such op" {
		t.Fatalf("message %q", rep.Error.Message)
	}
	wantErr(t, h.call(h.req(4, "send_text", `{"thread_id":"1","text":"x"}`)), 4, "not_logged_in")
	wantOK(t, h.call(h.req(5, "disconnect", "null")), 5)
}

func TestHelloWithStateOpensSession(t *testing.T) {
	h := start(t)
	h.ready()
	res := h.helloOK(goodState)
	acct, _ := res["account"].(map[string]any)
	if res["logged_in"] != true || acct["id"] != "1000123456789012" || acct["name"] != "" {
		t.Fatalf("unexpected hello result %v", res)
	}
	if len(h.f.opens) != 1 || h.f.opens[0].Cookies["xs"] != "XSVAL" || h.f.dataDirs[0] != h.dir {
		t.Fatalf("Open saw %+v in %v", h.f.opens, h.f.dataDirs)
	}
	wantOK(t, h.call(h.req(2, "connect", "null")), 2)
	wantErr(t, h.call(h.req(3, "login_start", `{"method":"cookies"}`)), 3, "already_logged_in")
	wantOK(t, h.call(h.req(4, "send_text", `{"thread_id":"1","text":"hi"}`)), 4)
}

func TestLoginStartAndFinishValidation(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK("null")
	wantErr(t, h.call(h.req(2, "login_finish", fmt.Sprintf(`{"cookies":%s}`, goodCookies))), 2, "bad_params")
	wantErr(t, h.call(h.req(3, "login_start", `{"method":"qr"}`)), 3, "bad_params")
	res := wantOK(t, h.call(h.req(4, "login_start", `{"method":"cookies"}`)), 4)
	names, _ := res["cookie_names"].([]any)
	needs, _ := res["needs"].([]any)
	if len(names) != 5 || names[0] != "c_user" || names[4] != "fr" || len(needs) != 1 || needs[0] != "cookies" ||
		!strings.Contains(res["instructions"].(string), "DevTools") {
		t.Fatalf("unexpected login_start result %v", res)
	}

	cases := []struct {
		id      int
		cookies string
		msg     string
	}{
		{10, `{"c_user":"1","datr":"SECRETVAL"}`, "missing cookie xs"},
		{11, `{"c_user":"1","xs":"","datr":"SECRETVAL"}`, "missing cookie xs"},
		{12, `{"xs":"SECRETVAL","datr":"SECRETVAL"}`, "missing cookie c_user"},
		{13, `{"c_user":"12ab","xs":"SECRETVAL","datr":"SECRETVAL"}`, "invalid cookie c_user"},
		{14, `{"c_user":"1","xs":"SECRET;VAL","datr":"d"}`, "invalid cookie xs"},
		{15, `{"c_user":"1","xs":"SECRET VAL","datr":"d"}`, "invalid cookie xs"},
		{16, `{"c_user":"1","xs":"SECRET,VAL","datr":"d"}`, "invalid cookie xs"},
		{17, `{"c_user":"1","xs":"x","datr":"d","b ad":"SECRETVAL"}`, "invalid cookie name"},
		{18, `{"c_user":"1","xs":"` + strings.Repeat("a", 4097) + `","datr":"d"}`, "invalid cookie xs"},
	}
	for _, c := range cases {
		rep := h.call(h.req(c.id, "login_finish", fmt.Sprintf(`{"cookies":%s}`, c.cookies)))
		wantErr(t, rep, uint64(c.id), "bad_params")
		if rep.Error.Message != c.msg || strings.Contains(rep.Error.Message, "SECRETVAL") {
			t.Fatalf("case %d: message %q", c.id, rep.Error.Message)
		}
	}
	many := map[string]string{"c_user": "1", "xs": "x", "datr": "d"}
	for i := 0; i < 30; i++ {
		many[fmt.Sprintf("k%d", i)] = "v"
	}
	b, _ := json.Marshal(map[string]any{"cookies": many})
	wantErr(t, h.call(h.req(19, "login_finish", string(b))), 19, "bad_params")
	wantErr(t, h.call(h.req(20, "login_finish", `{"cookies":{"c_user":"1","xs":"x","datr":"d"},"user_agent":"bad`+"\\n"+`ua"}`)), 20, "bad_params")
	if len(h.f.opens) != 0 {
		t.Fatal("Open must not run for rejected cookies")
	}
}

func TestLoginFinishSuccess(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK("null")
	wantOK(t, h.call(h.req(2, "login_start", `{"method":"cookies"}`)), 2)
	rep := h.call(h.req(3, "login_finish", fmt.Sprintf(`{"cookies":%s,"user_agent":"Mozilla/5.0 Test"}`, goodCookies)))
	res := wantOK(t, rep, 3)
	acct, _ := res["account"].(map[string]any)
	if acct["id"] != "1000123456789012" || acct["name"] != "Test User" {
		t.Fatalf("unexpected result %v", res)
	}
	if len(h.events) != 2 || h.events[0].Event != "state_updated" || h.events[1].Event != "logged_in" {
		t.Fatalf("want state_updated then logged_in, got %+v", h.events)
	}
	var su struct {
		State api.State `json:"state"`
	}
	if err := json.Unmarshal(h.events[0].Data, &su); err != nil || su.State.FBID != "1000123456789012" || su.State.Cookies["xs"] != "XSSECRET" {
		t.Fatalf("state_updated payload: %v %+v", err, su)
	}
	if len(h.f.opens) != 1 {
		t.Fatalf("want one Open, got %d", len(h.f.opens))
	}
	got := h.f.opens[0]
	want := map[string]string{"c_user": "1000123456789012", "xs": "XSSECRET", "datr": "DATRSECRET", "sb": "SBSECRET", "fr": "FRSECRET"}
	if got.UserAgent != "Mozilla/5.0 Test" || got.FBID != "1000123456789012" || len(got.Cookies) != len(want) {
		t.Fatalf("Open saw %+v", got)
	}
	for k, v := range want {
		if got.Cookies[k] != v {
			t.Fatalf("cookie %s differs", k)
		}
	}
	wantErr(t, h.call(h.req(4, "login_start", `{"method":"cookies"}`)), 4, "already_logged_in")
	wantOK(t, h.call(h.req(5, "list_threads", "null")), 5)
}

func TestLoginFinishCheckpointThenRetry(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK("null")
	attempt := 0
	h.f.validate = func(context.Context) (api.Account, error) {
		attempt++
		if attempt == 1 {
			return api.Account{}, &api.Error{Code: api.CodeCheckpointRequired, Msg: "checkpoint"}
		}
		return api.Account{ID: "1000123456789012", Name: "N"}, nil
	}
	wantOK(t, h.call(h.req(2, "login_start", `{"method":"cookies"}`)), 2)
	rep := h.call(h.req(3, "login_finish", fmt.Sprintf(`{"cookies":%s}`, goodCookies)))
	wantErr(t, rep, 3, "checkpoint_required")
	if rep.Error.Message != "checkpoint" || len(h.events) != 0 {
		t.Fatalf("unexpected error reply or events: %+v %+v", rep.Error, h.events)
	}
	if first := h.f.sessions[0]; first.closed.Load() != 1 {
		t.Fatal("failed session must be closed")
	}
	wantErr(t, h.call(h.req(4, "list_threads", "null")), 4, "not_logged_in")
	wantOK(t, h.call(h.req(5, "login_finish", fmt.Sprintf(`{"cookies":%s}`, goodCookies))), 5)
	if len(h.f.opens) != 2 {
		t.Fatalf("want two Opens, got %d", len(h.f.opens))
	}
}

func TestLoginFinishUnknownErrorIsInternal(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK("null")
	h.f.validate = func(context.Context) (api.Account, error) {
		return api.Account{}, errors.New("raw failure with xs=SECRET")
	}
	wantOK(t, h.call(h.req(2, "login_start", `{"method":"cookies"}`)), 2)
	rep := h.call(h.req(3, "login_finish", fmt.Sprintf(`{"cookies":%s}`, goodCookies)))
	wantErr(t, rep, 3, "internal")
	if strings.Contains(rep.Error.Message, "SECRET") {
		t.Fatalf("leaked: %q", rep.Error.Message)
	}
}

func TestLoginCancelAndInProgress(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK("null")
	started := make(chan struct{})
	h.f.validate = func(ctx context.Context) (api.Account, error) {
		close(started)
		<-ctx.Done()
		return api.Account{}, ctx.Err()
	}
	wantOK(t, h.call(h.req(2, "login_start", `{"method":"cookies"}`)), 2)
	h.send(h.req(3, "login_finish", fmt.Sprintf(`{"cookies":%s}`, goodCookies)))
	<-started
	wantErr(t, h.call(h.req(4, "login_finish", fmt.Sprintf(`{"cookies":%s}`, goodCookies))), 4, "bad_params")
	wantErr(t, h.call(h.req(5, "login_start", `{"method":"cookies"}`)), 5, "bad_params")
	h.send(h.req(6, "login_cancel", "null"))
	var got = map[uint64]ipc.Reply{}
	for len(got) < 2 {
		rep := h.nextReply()
		got[rep.ID] = rep
	}
	wantOK(t, got[6], 6)
	wantErr(t, got[3], 3, "login_failed")
	wantErr(t, h.call(h.req(7, "login_finish", fmt.Sprintf(`{"cookies":%s}`, goodCookies))), 7, "bad_params")
	wantOK(t, h.call(h.req(8, "login_cancel", "null")), 8)
}

func TestLogoutThenNotLoggedIn(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK(goodState)
	wantOK(t, h.call(h.req(2, "logout", "null")), 2)
	if len(h.events) != 1 || h.events[0].Event != "state_updated" || !strings.Contains(string(h.events[0].Data), `"state":null`) {
		t.Fatalf("want state_updated null, got %+v", h.events)
	}
	sess := h.f.sessions[0]
	if sess.loggedOut.Load() != 1 || sess.closed.Load() != 1 {
		t.Fatalf("session logout/close counts: %d %d", sess.loggedOut.Load(), sess.closed.Load())
	}
	wantErr(t, h.call(h.req(3, "send_text", `{"thread_id":"1","text":"hi"}`)), 3, "not_logged_in")
	h.events = nil
	wantOK(t, h.call(h.req(4, "logout", "null")), 4)
	if len(h.events) != 1 || h.events[0].Event != "state_updated" {
		t.Fatalf("logout without session must still emit state_updated, got %+v", h.events)
	}
}

func TestLoggedOutEventDropsSession(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK(goodState)
	go h.f.emit(api.EvLoggedOut, api.LoggedOutEvent{Reason: "token_invalidated"})
	ev := h.nextEvent()
	if ev.Event != "logged_out" || !strings.Contains(string(ev.Data), "token_invalidated") {
		t.Fatalf("unexpected event %+v", ev)
	}
	wantErr(t, h.call(h.req(2, "list_threads", "null")), 2, "not_logged_in")
	deadline := time.Now().Add(2 * time.Second)
	for h.f.sessions[0].closed.Load() == 0 && time.Now().Before(deadline) {
		time.Sleep(5 * time.Millisecond)
	}
	if h.f.sessions[0].closed.Load() != 1 {
		t.Fatal("dropped session must be closed")
	}
	wantOK(t, h.call(h.req(3, "login_start", `{"method":"cookies"}`)), 3)
}

func TestOtherEventsForwardVerbatim(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK(goodState)
	go h.f.emit(api.EvConnection, api.ConnectionEvent{State: "connected"})
	ev := h.nextEvent()
	if ev.Event != "connection" || string(ev.Data) != `{"state":"connected"}` {
		t.Fatalf("unexpected event %+v", ev)
	}
	wantOK(t, h.call(h.req(2, "list_threads", "null")), 2)
}

func TestSessionOpValidation(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK(goodState)
	file := filepath.Join(h.dir, "pic.png")
	if err := os.WriteFile(file, []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	long := strings.Repeat("é", 20001)
	cases := []struct {
		op, params, code string
	}{
		{"list_threads", `{"limit":0}`, "bad_params"},
		{"list_threads", `{"limit":201}`, "bad_params"},
		{"list_threads", `{"limit":200,"cursor":"` + strings.Repeat("c", 4097) + `"}`, "bad_params"},
		{"list_threads", `{"limit":200}`, ""},
		{"list_threads", `null`, ""},
		{"fetch_messages", `{}`, "bad_params"},
		{"fetch_messages", `{"thread_id":"1","limit":101}`, "bad_params"},
		{"fetch_messages", `{"thread_id":"1","before_ts_ms":-1}`, "bad_params"},
		{"fetch_messages", `{"thread_id":"1","before_id":"` + strings.Repeat("b", 257) + `"}`, "bad_params"},
		{"fetch_messages", `{"thread_id":"1","before_ts_ms":5}`, ""},
		{"download_attachment", `{"thread_id":"1","message_id":"m","attachment_id":"a","dest_path":"rel.bin"}`, "bad_params"},
		{"download_attachment", fmt.Sprintf(`{"thread_id":"1","message_id":"m","attachment_id":"a","dest_path":%q}`, h.dir), "bad_params"},
		{"download_attachment", fmt.Sprintf(`{"thread_id":"1","message_id":"m","attachment_id":"a","dest_path":%q}`, h.dir+"/nodir/x.bin"), "bad_params"},
		{"download_attachment", fmt.Sprintf(`{"thread_id":"1","message_id":"m","dest_path":%q}`, h.dir+"/x.bin"), "bad_params"},
		{"download_attachment", fmt.Sprintf(`{"thread_id":"1","message_id":"m","attachment_id":"a","dest_path":%q}`, h.dir+"/x.bin"), ""},
		{"send_text", `{"thread_id":"1","text":""}`, "bad_params"},
		{"send_text", `{"thread_id":"","text":"x"}`, "bad_params"},
		{"send_text", `{"thread_id":"1","text":"` + long + `"}`, "bad_params"},
		{"send_text", `{"thread_id":"1","text":"` + strings.Repeat("é", 20000) + `"}`, ""},
		{"send_text", `{"thread_id":"1","text":"x","reply_to":"` + strings.Repeat("r", 257) + `"}`, "bad_params"},
		{"send_media", fmt.Sprintf(`{"thread_id":"1","path":%q,"mime":"image/png","kind":"image"}`, h.dir), "bad_params"},
		{"send_media", `{"thread_id":"1","path":"rel.png","mime":"image/png","kind":"image"}`, "bad_params"},
		{"send_media", fmt.Sprintf(`{"thread_id":"1","path":%q,"mime":"image/png","kind":"gif"}`, file), "bad_params"},
		{"send_media", fmt.Sprintf(`{"thread_id":"1","path":%q,"mime":"","kind":"image"}`, file), "bad_params"},
		{"send_media", fmt.Sprintf(`{"thread_id":"1","path":%q,"mime":"image/png","kind":"image","filename":"a/b.png"}`, file), "bad_params"},
		{"send_media", fmt.Sprintf(`{"thread_id":"1","path":%q,"mime":"image/png","kind":"image","caption":"hi","filename":"b.png"}`, file), ""},
		{"send_reaction", `{"thread_id":"1","emoji":"x"}`, "bad_params"},
		{"send_reaction", `{"thread_id":"1","message_id":"m","emoji":"` + strings.Repeat("e", 65) + `"}`, "bad_params"},
		{"send_reaction", `{"thread_id":"1","message_id":"m","emoji":""}`, ""},
		{"mark_read", `{"thread_id":"1","up_to_ts_ms":-1}`, "bad_params"},
		{"mark_read", `{"thread_id":"1"}`, ""},
		{"set_typing", `{"thread_id":"1"}`, "bad_params"},
		{"set_typing", `{"thread_id":"1","on":false}`, ""},
	}
	for i, c := range cases {
		id := 100 + i
		rep := h.call(h.req(id, c.op, c.params))
		if c.code == "" {
			wantOK(t, rep, uint64(id))
			continue
		}
		wantErr(t, rep, uint64(id), c.code)
		if strings.Contains(rep.Error.Message, "rel.") || strings.Contains(rep.Error.Message, h.dir) {
			t.Fatalf("%s: message echoes a value: %q", c.op, rep.Error.Message)
		}
	}
}

func TestNilSlicesBecomeArrays(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK(goodState)
	h.f.listThreads = func(context.Context, api.ListThreadsParams) (api.ListThreadsResult, error) {
		return api.ListThreadsResult{Threads: []api.Thread{{ID: "1", Kind: api.KindDM}}}, nil
	}
	rep := h.call(h.req(2, "list_threads", `{"limit":10}`))
	if !strings.Contains(string(rep.Result), `"participants":[]`) {
		t.Fatalf("participants not an array: %s", rep.Result)
	}
	h.f.listThreads = nil
	rep = h.call(h.req(3, "list_threads", "null"))
	if string(rep.Result) != `{"threads":[]}` {
		t.Fatalf("unexpected result %s", rep.Result)
	}
	rep = h.call(h.req(4, "fetch_messages", `{"thread_id":"1"}`))
	if string(rep.Result) != `{"messages":[],"has_more":false}` {
		t.Fatalf("unexpected result %s", rep.Result)
	}
}

func TestEngineErrorPassesThrough(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK(goodState)
	h.f.sendText = func(context.Context, api.SendTextParams) (api.SendResult, error) {
		return api.SendResult{}, api.Errorf(api.CodeSendUnknown, "outcome unknown")
	}
	rep := h.call(h.req(2, "send_text", `{"thread_id":"1","text":"hi"}`))
	wantErr(t, rep, 2, "send_unknown")
	if rep.Error.Message != "outcome unknown" {
		t.Fatalf("message %q", rep.Error.Message)
	}
}

func TestSlowOpDoesNotBlockPing(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK(goodState)
	release := make(chan struct{})
	h.f.listThreads = func(context.Context, api.ListThreadsParams) (api.ListThreadsResult, error) {
		<-release
		return api.ListThreadsResult{}, nil
	}
	h.send(h.req(2, "list_threads", "null"))
	wantOK(t, h.call(h.req(3, "ping", "null")), 3)
	wantOK(t, h.call(h.req(4, "disconnect", "null")), 4)
	close(release)
	wantOK(t, h.nextReply(), 2)
}

func TestShutdownClosesSession(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK(goodState)
	wantOK(t, h.call(h.req(2, "shutdown", "null")), 2)
	if err := h.waitDone(); err != nil {
		t.Fatalf("Run returned %v", err)
	}
	sess := h.f.sessions[0]
	if sess.closed.Load() != 1 || sess.disconnected.Load() != 1 {
		t.Fatalf("close %d disconnect %d", sess.closed.Load(), sess.disconnected.Load())
	}
}

func TestMalformedFrameStopsLoop(t *testing.T) {
	h := start(t)
	h.ready()
	h.send("this is not json")
	if err := h.waitDone(); !errors.Is(err, ErrFrame) {
		t.Fatalf("want ErrFrame, got %v", err)
	}
}

func TestOversizedFrameStopsLoop(t *testing.T) {
	h := start(t)
	h.ready()
	go func() {
		_, _ = io.WriteString(h.toSrv, strings.Repeat("a", ipc.MaxLine+1)+"\n")
	}()
	if err := h.waitDone(); !errors.Is(err, ErrFrame) {
		t.Fatalf("want ErrFrame, got %v", err)
	}
}

func TestEOFEndsLoopCleanly(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK(goodState)
	h.toSrv.Close()
	if err := h.waitDone(); err != nil {
		t.Fatalf("Run returned %v", err)
	}
	if h.f.sessions[0].closed.Load() != 1 {
		t.Fatal("session must be closed on EOF")
	}
}

func TestContextCancelEndsLoop(t *testing.T) {
	inR, inW := io.Pipe()
	defer inW.Close()
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- Run(ctx, inR, io.Discard, zerolog.Nop(), &fakeFactory{}) }()
	cancel()
	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("want context.Canceled, got %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Run did not return after cancel")
	}
}

func TestHandlerPanicBecomesInternalReply(t *testing.T) {
	h := startWith(t, &testHooks{extra: map[string]handler{
		"boom": func(context.Context, ipc.Request) ipc.Reply { panic("SECRET panic value") },
	}})
	h.ready()
	h.helloOK("null")
	rep := h.call(h.req(2, "boom", "null"))
	wantErr(t, rep, 2, "internal")
	if rep.Error.Message != "internal error" {
		t.Fatalf("message %q", rep.Error.Message)
	}
	wantOK(t, h.call(h.req(3, "ping", "null")), 3)
	wantOK(t, h.call(h.req(4, "login_start", `{"method":"cookies"}`)), 4)
}

func TestLoginFinishPanicResetsState(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK("null")
	first := true
	h.f.validate = func(context.Context) (api.Account, error) {
		if first {
			first = false
			panic("SECRET")
		}
		return api.Account{ID: "1000123456789012"}, nil
	}
	wantOK(t, h.call(h.req(2, "login_start", `{"method":"cookies"}`)), 2)
	rep := h.call(h.req(3, "login_finish", fmt.Sprintf(`{"cookies":%s}`, goodCookies)))
	wantErr(t, rep, 3, "internal")
	if h.f.sessions[0].closed.Load() != 1 {
		t.Fatal("session of a panicked finish must be closed")
	}
	wantOK(t, h.call(h.req(4, "login_finish", fmt.Sprintf(`{"cookies":%s}`, goodCookies))), 4)
	wantOK(t, h.call(h.req(5, "ping", "null")), 5)
}

func TestLoginFinishFailClosedEmitsLoggedOut(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK("null")
	attempt := 0
	h.f.validate = func(context.Context) (api.Account, error) {
		attempt++
		if attempt > 1 {
			return api.Account{ID: "1000123456789012"}, nil
		}
		return api.Account{}, &api.Error{Code: api.CodeCheckpointRequired, Msg: "checkpoint", Reason: "checkpoint_required", URL: "https://www.facebook.com/checkpoint/1"}
	}
	wantOK(t, h.call(h.req(2, "login_start", `{"method":"cookies"}`)), 2)
	rep := h.call(h.req(3, "login_finish", fmt.Sprintf(`{"cookies":%s}`, goodCookies)))
	wantErr(t, rep, 3, "checkpoint_required")
	if strings.Contains(rep.Error.Message, "http") {
		t.Fatalf("error message carries the URL: %q", rep.Error.Message)
	}
	if len(h.events) != 1 || h.events[0].Event != "logged_out" {
		t.Fatalf("want one logged_out before the reply, got %+v", h.events)
	}
	var d api.LoggedOutEvent
	if err := json.Unmarshal(h.events[0].Data, &d); err != nil || d.Reason != "checkpoint_required" || d.URL != "https://www.facebook.com/checkpoint/1" {
		t.Fatalf("logged_out payload: %v %+v", err, d)
	}
	wantOK(t, h.call(h.req(4, "login_finish", fmt.Sprintf(`{"cookies":%s}`, goodCookies))), 4)
}

func TestLoginFinishWithoutReasonEmitsNoLoggedOut(t *testing.T) {
	h := start(t)
	h.ready()
	h.helloOK("null")
	h.f.validate = func(context.Context) (api.Account, error) {
		return api.Account{}, api.Errorf(api.CodeLoginFailed, "bad cookies")
	}
	wantOK(t, h.call(h.req(2, "login_start", `{"method":"cookies"}`)), 2)
	wantErr(t, h.call(h.req(3, "login_finish", fmt.Sprintf(`{"cookies":%s}`, goodCookies))), 3, "login_failed")
	if len(h.events) != 0 {
		t.Fatalf("unexpected events %+v", h.events)
	}
}

func TestCancelBetweenValidateAndInstall(t *testing.T) {
	for _, op := range []string{"login_cancel", "logout"} {
		reached := make(chan struct{})
		proceed := make(chan struct{})
		h := startWith(t, &testHooks{afterValidate: func() {
			close(reached)
			<-proceed
		}})
		h.ready()
		h.helloOK("null")
		wantOK(t, h.call(h.req(2, "login_start", `{"method":"cookies"}`)), 2)
		h.send(h.req(3, "login_finish", fmt.Sprintf(`{"cookies":%s}`, goodCookies)))
		<-reached
		wantOK(t, h.call(h.req(4, op, "null")), 4)
		h.events = nil
		close(proceed)
		wantErr(t, h.nextReply(), 3, "login_failed")
		for _, ev := range h.events {
			if ev.Event == "logged_in" {
				t.Fatalf("%s: logged_in emitted after cancel", op)
			}
		}
		if h.f.sessions[0].closed.Load() == 0 {
			t.Fatalf("%s: session not closed", op)
		}
		wantErr(t, h.call(h.req(5, "list_threads", "null")), 5, "not_logged_in")
	}
}
