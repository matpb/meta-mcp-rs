package engine

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"go.mau.fi/mautrix-meta/pkg/messagix/socket"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
	"go.mau.fi/whatsmeow/types"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

const (
	newID      = "5555"
	notAPerson = "not a person you can message"
)

// newChatSession is sendSession plus the event recorder, with the encrypted client reported up.
func newChatSession(t *testing.T) (*session, *fakeClient, *recorder) {
	t.Helper()
	fc := newFakeClient(nil)
	s, rec := openTest(t, fc)
	if _, err := s.Validate(t.Context()); err != nil {
		t.Fatal(err)
	}
	s.processTable(threadsTable(), modeInitial)
	fc.mu.Lock()
	fc.connected = true
	fc.mu.Unlock()
	setE2EEUp(t, true)
	return s, fc, rec
}

func setE2EEUp(t *testing.T, up bool) {
	t.Helper()
	old := e2eeUp
	e2eeUp = func(*session) bool { return up }
	t.Cleanup(func() { e2eeUp = old })
}

func personRow(id, name string) *table.LSInsertSearchResult {
	return &table.LSInsertSearchResult{ResultId: id, Type_: table.SearchTypeContact, ThreadType: table.ONE_TO_ONE, CanViewerMessage: true, DisplayName: name}
}

// newChatExec answers a SearchUserTask with rows, the WhatsApp create with one subtask, and the rest with an empty table.
func newChatExec(rows ...*table.LSInsertSearchResult) func(context.Context, ...socket.Task) (*table.LSTable, error) {
	return func(_ context.Context, tasks ...socket.Task) (*table.LSTable, error) {
		switch tasks[0].(type) {
		case *socket.SearchUserTask:
			return &table.LSTable{LSInsertSearchResult: rows}, nil
		case *socket.CreateWhatsAppThreadTask:
			return &table.LSTable{LSIssueNewTask: []*table.LSIssueNewTask{{}}}, nil
		}
		return &table.LSTable{}, nil
	}
}

// replyWithEmptySearch answers a user search with no results and any other task like replyWith.
func replyWithEmptySearch(mid string) func(context.Context, ...socket.Task) (*table.LSTable, error) {
	reply := replyWith(mid)
	return func(ctx context.Context, tasks ...socket.Task) (*table.LSTable, error) {
		if _, ok := tasks[0].(*socket.SearchUserTask); ok {
			return &table.LSTable{}, nil
		}
		return reply(ctx, tasks...)
	}
}

func taskTypes(fc *fakeClient) []string {
	fc.mu.Lock()
	defer fc.mu.Unlock()
	out := make([]string, len(fc.tasks))
	for i, task := range fc.tasks {
		out[i] = fmt.Sprintf("%T", task)
	}
	return out
}

func countTasks[T socket.Task](fc *fakeClient) int {
	fc.mu.Lock()
	defer fc.mu.Unlock()
	n := 0
	for _, task := range fc.tasks {
		if _, ok := task.(T); ok {
			n++
		}
	}
	return n
}

func wantTasks(t *testing.T, fc *fakeClient, want ...string) {
	t.Helper()
	if got := taskTypes(fc); !reflect.DeepEqual(got, want) && (len(got) != 0 || len(want) != 0) {
		t.Fatalf("executed tasks %v, want %v", got, want)
	}
}

func threadEvents(rec *recorder, id string) []api.ThreadEvent {
	var out []api.ThreadEvent
	for _, e := range rec.named(api.EvThread) {
		if ev, ok := e.data.(api.ThreadEvent); ok && ev.Thread.ID == id {
			out = append(out, ev)
		}
	}
	return out
}

var createFlow = []string{
	"*socket.SearchUserTask", "*socket.CreateThreadTask", "*socket.CreateWhatsAppThreadTask", "*table.LSIssueNewTask",
}

func wantOpened(t *testing.T, s *session, rec *recorder) {
	t.Helper()
	tg, err := s.resolveThread(newID)
	if err != nil {
		t.Fatalf("resolveThread: %v", err)
	}
	if !tg.enc || tg.chat != (types.JID{User: newID, Server: types.MessengerServer}) {
		t.Fatalf("target %+v", tg)
	}
	evs := threadEvents(rec, newID)
	if len(evs) == 0 || !evs[0].Thread.Encrypted || evs[0].Thread.Kind != api.KindDM {
		t.Fatalf("thread events %+v", evs)
	}
}

func wantCreateTasks(t *testing.T, fc *fakeClient) {
	t.Helper()
	fc.mu.Lock()
	defer fc.mu.Unlock()
	if len(fc.tasks) != len(createFlow) {
		t.Fatalf("tasks %v", fc.tasks)
	}
	if st := fc.tasks[0].(*socket.SearchUserTask); st.Query != newID {
		t.Fatalf("search %+v", st)
	}
	if ct := fc.tasks[1].(*socket.CreateThreadTask); ct.ThreadFBID != 5555 || ct.SyncGroup != 1 {
		t.Fatalf("create thread %+v", ct)
	}
	wa := fc.tasks[2].(*socket.CreateWhatsAppThreadTask)
	if wa.WAJID != 5555 || wa.ThreadType != table.ENCRYPTED_OVER_WA_ONE_TO_ONE || wa.FolderType != table.INBOX {
		t.Fatalf("create whatsapp thread %+v", wa)
	}
}

func TestNewChatSendTextOpensEncryptedDM(t *testing.T) {
	s, fc, rec := newChatSession(t)
	fc.exec = newChatExec(personRow(newID, "Pat"))
	_, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: newID, Text: "hello"})
	wantCode(t, err, api.CodeNotConnected)
	wantTasks(t, fc, createFlow...)
	wantCreateTasks(t, fc)
	if countTasks[*socket.SendMessageTask](fc) != 0 {
		t.Fatal("a plaintext send was executed")
	}
	wantOpened(t, s, rec)

	_, err = s.SendText(t.Context(), api.SendTextParams{ThreadID: newID, Text: "again"})
	wantCode(t, err, api.CodeNotConnected)
	wantTasks(t, fc, createFlow...)
}

func TestNewChatCanonicalAliasSearchesCanonicalID(t *testing.T) {
	s, fc, _ := newChatSession(t)
	fc.exec = newChatExec(personRow(newID, "Pat"))
	_, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: "+05555", Text: "hello"})
	wantCode(t, err, api.CodeNotConnected)
	wantTasks(t, fc, createFlow...)
	wantCreateTasks(t, fc)
}

func TestNewChatSendMediaOpensEncryptedDM(t *testing.T) {
	s, fc, rec := newChatSession(t)
	fc.exec = newChatExec(personRow(newID, "Pat"))
	path := filepath.Join(t.TempDir(), "pic.png")
	if err := os.WriteFile(path, encodePNG(t, gradient(8, 8)), 0o600); err != nil {
		t.Fatal(err)
	}
	_, err := s.SendMedia(t.Context(), api.SendMediaParams{ThreadID: newID, Path: path, Mime: "image/png", Kind: "image"})
	wantCode(t, err, api.CodeNotConnected)
	wantTasks(t, fc, createFlow...)
	wantCreateTasks(t, fc)
	if countTasks[*socket.SendMessageTask](fc) != 0 {
		t.Fatal("a plaintext send was executed")
	}
	wantOpened(t, s, rec)
}

func TestNewChatVettedBySearchSkipsSecondSearch(t *testing.T) {
	s, fc, rec := newChatSession(t)
	fc.exec = newChatExec(personRow(newID, "Pat"))
	res, err := s.SearchUsers(t.Context(), api.SearchUsersParams{Query: "pat", Limit: 10})
	if err != nil || len(res.Users) != 1 || res.Users[0].ID != newID {
		t.Fatalf("res %+v err %v", res, err)
	}
	_, err = s.SendText(t.Context(), api.SendTextParams{ThreadID: newID, Text: "hello"})
	wantCode(t, err, api.CodeNotConnected)
	wantTasks(t, fc, "*socket.SearchUserTask", "*socket.CreateThreadTask", "*socket.CreateWhatsAppThreadTask", "*table.LSIssueNewTask")
	if st := fc.tasks[0].(*socket.SearchUserTask); st.Query != "pat" {
		t.Fatalf("the only search must be the user's, got %q", st.Query)
	}
	wantOpened(t, s, rec)
}

func TestNewChatRefusals(t *testing.T) {
	cases := []struct {
		name   string
		target string
		rows   []*table.LSInsertSearchResult
		nilTbl bool
	}{
		{"cannot message, no other user", newID, []*table.LSInsertSearchResult{
			{ResultId: newID, Type_: table.SearchTypeContact, ThreadType: table.ONE_TO_ONE}}, false},
		{"blocked by viewer", newID, []*table.LSInsertSearchResult{
			{ResultId: "9", Type_: table.SearchTypeContact, ThreadType: table.ONE_TO_ONE, OtherUserId: 5555, BlockedByViewerStatus: 1}}, false},
		{"page", newID, []*table.LSInsertSearchResult{
			{ResultId: newID, Type_: table.SearchTypePage, ThreadType: table.ONE_TO_ONE, CanViewerMessage: true}}, false},
		{"ai bot", newID, []*table.LSInsertSearchResult{
			{ResultId: newID, Type_: table.SearchTypeAIBot, ThreadType: table.AI_BOT, CanViewerMessage: true}}, false},
		{"group thread type", newID, []*table.LSInsertSearchResult{
			{ResultId: newID, Type_: table.SearchTypeContact, ThreadType: table.GROUP_THREAD, CanViewerMessage: true}}, false},
		{"group search type", newID, []*table.LSInsertSearchResult{
			{ResultId: newID, Type_: table.SearchTypeGroup, ThreadType: table.GROUP_THREAD, CanViewerMessage: true}}, false},
		{"different id", newID, []*table.LSInsertSearchResult{personRow("6666", "Sam")}, false},
		{"own id", "111", []*table.LSInsertSearchResult{personRow("111", "Me")}, false},
		{"empty table", newID, nil, false},
		{"nil table", newID, nil, true},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			s, fc, rec := newChatSession(t)
			fc.exec = func(_ context.Context, tasks ...socket.Task) (*table.LSTable, error) {
				if c.nilTbl {
					return nil, nil
				}
				return &table.LSTable{LSInsertSearchResult: c.rows}, nil
			}
			_, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: c.target, Text: "hello"})
			ae := wantCode(t, err, api.CodeThreadNotFound)
			if !strings.Contains(ae.Msg, notAPerson) {
				t.Fatalf("message %q", ae.Msg)
			}
			wantTasks(t, fc, "*socket.SearchUserTask")
			if _, ok := s.threadInfo(c.target); ok && c.target == newID {
				t.Fatal("a refused id reached the registry")
			}
			if _, ok := s.people.get(5555); ok {
				t.Fatal("a refused id was remembered as vetted")
			}
			if len(threadEvents(rec, c.target)) != 0 {
				t.Fatal("a refused id emitted a thread event")
			}
		})
	}
}

func TestNewChatRefusedForMedia(t *testing.T) {
	s, fc, _ := newChatSession(t)
	fc.exec = newChatExec(personRow("6666", "Sam"))
	_, err := s.SendMedia(t.Context(), api.SendMediaParams{ThreadID: newID, Path: "/nonexistent.png", Mime: "image/png", Kind: "image"})
	ae := wantCode(t, err, api.CodeThreadNotFound)
	if !strings.Contains(ae.Msg, notAPerson) {
		t.Fatalf("message %q", ae.Msg)
	}
	wantTasks(t, fc, "*socket.SearchUserTask")
}

func TestNewChatEncryptedClientDownCreatesNothing(t *testing.T) {
	s, fc, rec := newChatSession(t)
	setE2EEUp(t, false)
	fc.exec = newChatExec(personRow(newID, "Pat"))
	_, err := s.SendText(t.Context(), api.SendTextParams{ThreadID: newID, Text: "hello"})
	wantCode(t, err, api.CodeNotConnected)
	wantTasks(t, fc, "*socket.SearchUserTask")
	if _, ok := s.threadInfo(newID); ok {
		t.Fatal("the thread was registered without an encrypted client")
	}
	if len(threadEvents(rec, newID)) != 0 {
		t.Fatal("a thread event was emitted")
	}
}

func TestNewChatOtherOpsAndBadIDsNeverOpenAThread(t *testing.T) {
	s, fc, rec := newChatSession(t)
	fc.exec = newChatExec(personRow(newID, "Pat"))
	ctx := t.Context()
	errs := map[string]error{}
	errs["reaction"] = s.SendReaction(ctx, api.SendReactionParams{ThreadID: newID, MessageID: "mid.$x", Emoji: "x"})
	errs["mark read"] = s.MarkRead(ctx, api.MarkReadParams{ThreadID: newID})
	errs["typing"] = s.SetTyping(ctx, api.SetTypingParams{ThreadID: newID, On: true})
	_, errs["reply"] = s.SendText(ctx, api.SendTextParams{ThreadID: newID, Text: "hi", ReplyTo: "mid.$x"})
	for _, id := range []string{"abc", "-5", "0"} {
		_, errs["text to "+id] = s.SendText(ctx, api.SendTextParams{ThreadID: id, Text: "hi"})
	}
	for name, err := range errs {
		var ae *api.Error
		if !errors.As(err, &ae) || ae.Code != api.CodeThreadNotFound {
			t.Fatalf("%s: err %v, want thread_not_found", name, err)
		}
	}
	wantTasks(t, fc)
	if _, ok := s.threadInfo(newID); ok || len(threadEvents(rec, newID)) != 0 {
		t.Fatal("a thread was opened")
	}
}

func searchRows() []*table.LSInsertSearchResult {
	return []*table.LSInsertSearchResult{
		personRow(newID, "Pat"),
		{ResultId: newID, Type_: table.SearchTypeNonContact, ThreadType: table.ONE_TO_ONE, CanViewerMessage: true, DisplayName: "Pat Duplicate"},
		personRow("222", "Alice"),
		{ResultId: "7777", Type_: table.SearchTypePage, ThreadType: table.ONE_TO_ONE, CanViewerMessage: true, DisplayName: "A Page"},
		{ResultId: "4000", Type_: table.SearchTypeGroup, ThreadType: table.GROUP_THREAD, CanViewerMessage: true, DisplayName: "A Group"},
		{ResultId: "1", Type_: table.SearchTypeNonContact, ThreadType: table.ONE_TO_ONE, OtherUserId: 8888, DisplayName: "Sam"},
		{ResultId: "2", Type_: table.SearchTypeContact, ThreadType: table.ONE_TO_ONE, DisplayName: "Cannot Message"},
	}
}

func TestSearchUsersShape(t *testing.T) {
	s, fc, _ := newChatSession(t)
	fc.exec = newChatExec(searchRows()...)
	res, err := s.SearchUsers(t.Context(), api.SearchUsersParams{Query: "pat", Limit: 10})
	if err != nil {
		t.Fatal(err)
	}
	want := []api.Person{
		{ID: newID, Name: "Pat", HasThread: false},
		{ID: "222", Name: "Alice", HasThread: true},
		{ID: "8888", Name: "Sam", HasThread: false},
	}
	if !reflect.DeepEqual(res.Users, want) {
		t.Fatalf("users %+v, want %+v", res.Users, want)
	}

	res, err = s.SearchUsers(t.Context(), api.SearchUsersParams{Query: "pat", Limit: 2})
	if err != nil || !reflect.DeepEqual(res.Users, want[:2]) {
		t.Fatalf("limited users %+v err %v", res.Users, err)
	}

	fc.mu.Lock()
	st := fc.tasks[0].(*socket.SearchUserTask)
	fc.mu.Unlock()
	if st.Query != "pat" || st.SurfaceType != 5 || !reflect.DeepEqual(st.SupportedTypes, searchTypes) {
		t.Fatalf("search task %+v", st)
	}
	if !containsType(st.SupportedTypes, table.SearchTypeContact) || !containsType(st.SupportedTypes, table.SearchTypeNonContact) {
		t.Fatalf("supported types %v", st.SupportedTypes)
	}
}

func containsType(list []table.SearchType, want table.SearchType) bool {
	for _, v := range list {
		if v == want {
			return true
		}
	}
	return false
}

func TestSearchUsersEmptyIsNonNilSlice(t *testing.T) {
	for name, exec := range map[string]func(context.Context, ...socket.Task) (*table.LSTable, error){
		"no rows":      newChatExec(),
		"only refused": newChatExec(&table.LSInsertSearchResult{ResultId: "7777", Type_: table.SearchTypePage, ThreadType: table.ONE_TO_ONE, CanViewerMessage: true}),
		"nil table":    func(context.Context, ...socket.Task) (*table.LSTable, error) { return nil, nil },
	} {
		s, fc, _ := newChatSession(t)
		fc.exec = exec
		res, err := s.SearchUsers(t.Context(), api.SearchUsersParams{Query: "zzz", Limit: 10})
		if err != nil || res.Users == nil || len(res.Users) != 0 {
			t.Fatalf("%s: users %#v err %v", name, res.Users, err)
		}
	}
}

func TestSearchUsersErrors(t *testing.T) {
	s, fc, _ := newChatSession(t)
	fc.exec = func(context.Context, ...socket.Task) (*table.LSTable, error) { return nil, errors.New("boom secret") }
	_, err := s.SearchUsers(t.Context(), api.SearchUsersParams{Query: "pat", Limit: 10})
	ae := wantCode(t, err, api.CodeIO)
	if strings.Contains(ae.Msg, "secret") {
		t.Fatalf("message %q", ae.Msg)
	}

	fc.mu.Lock()
	fc.connected = false
	fc.mu.Unlock()
	_, err = s.SearchUsers(t.Context(), api.SearchUsersParams{Query: "pat", Limit: 10})
	wantCode(t, err, api.CodeNotConnected)
}

func TestRegistryOpenDMUpgradesPlaintextEntry(t *testing.T) {
	s, _, _ := newChatSession(t)
	before, ok := s.reg.entry("222")
	if !ok || before.encrypted() || before.Kind != api.KindDM {
		t.Fatalf("seed entry %+v", before)
	}
	if !s.reg.openDM("222", 222) {
		t.Fatal("upgrade must report a change")
	}
	e, _ := s.reg.entry("222")
	if !e.encrypted() || e.Kind != api.KindDM || e.WAServer != types.MessengerServer || e.ThreadType != table.ENCRYPTED_OVER_WA_ONE_TO_ONE {
		t.Fatalf("upgraded entry %+v", e)
	}
	if th, _ := s.reg.snapshot("222"); !th.Encrypted {
		t.Fatalf("snapshot %+v", th)
	}
	if s.reg.openDM("222", 222) {
		t.Fatal("a second openDM must report no change")
	}

	s.reg.ingest(&table.LSTable{LSDeleteThenInsertThread: []*table.LSDeleteThenInsertThread{
		{ThreadKey: 222, ThreadType: table.ONE_TO_ONE, Snippet: "later", LastActivityTimestampMs: 9_000_000},
	}})
	e, _ = s.reg.entry("222")
	if !e.encrypted() || e.ThreadType != table.ENCRYPTED_OVER_WA_ONE_TO_ONE || e.WAServer != types.MessengerServer || e.Preview != "later" {
		t.Fatalf("entry after a plaintext ingest %+v", e)
	}
	if th, _ := s.reg.snapshot("222"); !th.Encrypted {
		t.Fatalf("snapshot after a plaintext ingest %+v", th)
	}
}

func TestRegistryOpenDMNewEntrySurvivesPlaintextIngest(t *testing.T) {
	s, _, _ := newChatSession(t)
	if !s.reg.openDM(newID, 5555) {
		t.Fatal("a new entry must report a change")
	}
	s.reg.ingest(&table.LSTable{LSDeleteThenInsertThread: []*table.LSDeleteThenInsertThread{
		{ThreadKey: 5555, ThreadType: table.ONE_TO_ONE, Snippet: "hey"},
	}})
	e, ok := s.reg.entry(newID)
	if !ok || !e.encrypted() || e.FBThreadKey != 5555 || e.Kind != api.KindDM || e.Preview != "hey" {
		t.Fatalf("entry %+v", e)
	}
}
