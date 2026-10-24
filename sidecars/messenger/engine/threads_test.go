package engine

import (
	"context"
	"encoding/base64"
	"os"
	"path/filepath"
	"testing"
	"time"

	"go.mau.fi/mautrix-meta/pkg/messagix/socket"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

const (
	alice = int64(222)
	bob   = int64(333)
)

// threadsTable holds a 1:1 (newest), a plain group and a hybrid encrypted group.
func threadsTable() *table.LSTable {
	return &table.LSTable{
		LSDeleteThenInsertContact: []*table.LSDeleteThenInsertContact{
			{Id: alice, Name: "Alice"},
			{Id: bob, Name: "Bob"},
		},
		LSVerifyHybridThreadExists: []*table.LSVerifyHybridThreadExists{
			{ThreadKey: 5000, ThreadJID: 9000, ThreadType: table.GROUP_THREAD},
		},
		LSDeleteThenInsertThread: []*table.LSDeleteThenInsertThread{
			{ThreadKey: alice, ThreadType: table.ONE_TO_ONE, Snippet: "hi there", LastActivityTimestampMs: 3_000_000, LastReadWatermarkTimestampMs: 1_000_000},
			{ThreadKey: 4000, ThreadType: table.GROUP_THREAD, ThreadName: "Crew", Snippet: "lunch?", LastActivityTimestampMs: 2_000_000, LastReadWatermarkTimestampMs: 2_000_000},
			{ThreadKey: 5000, ThreadType: table.ENCRYPTED_OVER_WA_GROUP, ThreadName: "Secret", Snippet: "shh", LastActivityTimestampMs: 1_000_000, LastReadWatermarkTimestampMs: 0},
		},
		LSAddParticipantIdToGroupThread: []*table.LSAddParticipantIdToGroupThread{
			{ThreadKey: 4000, ContactId: testMe},
			{ThreadKey: 4000, ContactId: alice},
			{ThreadKey: 4000, ContactId: bob},
			{ThreadKey: 5000, ContactId: testMe},
			{ThreadKey: 5000, ContactId: bob},
		},
	}
}

func listAll(t *testing.T, s *session, limit int) []api.ListThreadsResult {
	t.Helper()
	var pages []api.ListThreadsResult
	cursor := ""
	for i := 0; i < 10; i++ {
		res, err := s.ListThreads(t.Context(), api.ListThreadsParams{Limit: limit, Cursor: cursor})
		if err != nil {
			t.Fatal(err)
		}
		pages = append(pages, res)
		if res.Cursor == "" {
			return pages
		}
		cursor = res.Cursor
	}
	t.Fatal("cursor never ended")
	return nil
}

func TestThreadRegistryIngest(t *testing.T) {
	s, rec := openTest(t, newFakeClient(nil))
	s.processTable(threadsTable(), modeInitial)

	pages := listAll(t, s, 10)
	if len(pages) != 1 || len(pages[0].Threads) != 3 {
		t.Fatalf("pages %+v", pages)
	}
	th := pages[0].Threads
	if th[0].ID != "222" || th[1].ID != "4000" || th[2].ID != "9000" {
		t.Fatalf("order %s %s %s", th[0].ID, th[1].ID, th[2].ID)
	}

	dm := th[0]
	if dm.Kind != api.KindDM || dm.Encrypted || dm.Name != nil {
		t.Fatalf("dm %+v", dm)
	}
	if dm.LastMessageTS == nil || *dm.LastMessageTS != 3000 || dm.LastMessagePreview == nil || *dm.LastMessagePreview != "hi there" {
		t.Fatalf("dm activity %+v", dm)
	}
	if dm.Unread == nil || !*dm.Unread {
		t.Fatalf("dm unread %v", dm.Unread)
	}
	if len(dm.Participants) != 2 || !dm.Participants[0].IsMe || dm.Participants[0].ID != "111" || dm.Participants[1].ID != "222" || dm.Participants[1].IsMe || dm.Participants[1].Name != "Alice" {
		t.Fatalf("dm participants %+v", dm.Participants)
	}

	group := th[1]
	if group.Kind != api.KindGroup || group.Encrypted || group.Name == nil || *group.Name != "Crew" {
		t.Fatalf("group %+v", group)
	}
	if group.Unread == nil || *group.Unread {
		t.Fatalf("group unread %v", group.Unread)
	}
	if len(group.Participants) != 3 || !group.Participants[0].IsMe || group.Participants[2].Name != "Bob" {
		t.Fatalf("group participants %+v", group.Participants)
	}

	hybrid := th[2]
	if hybrid.Kind != api.KindGroup || !hybrid.Encrypted || hybrid.ID != "9000" {
		t.Fatalf("hybrid %+v", hybrid)
	}
	e, ok := s.threadInfo("9000")
	if !ok || e.FBThreadKey != 5000 || e.WAServer != "g.us" || e.ThreadType != table.ENCRYPTED_OVER_WA_GROUP {
		t.Fatalf("hybrid entry %+v", e)
	}
	if _, ok := s.threadInfo("5000"); ok {
		t.Fatal("hybrid thread must not stay under its facebook key")
	}
	if got := s.reg.threadID(5000); got != "9000" {
		t.Fatalf("mapped id %s", got)
	}

	if n := len(rec.named(api.EvThread)); n != 3 {
		t.Fatalf("want 3 thread events, got %d", n)
	}
	s.processTable(threadsTable(), modeInitial)
	if n := len(rec.named(api.EvThread)); n != 3 {
		t.Fatalf("re-ingesting identical rows must stay quiet, got %d events", n)
	}
}

func TestListThreadsCursorPaging(t *testing.T) {
	s, _ := openTest(t, newFakeClient(nil))
	s.processTable(threadsTable(), modeInitial)

	pages := listAll(t, s, 2)
	if len(pages) != 2 {
		t.Fatalf("want 2 pages, got %d", len(pages))
	}
	if len(pages[0].Threads) != 2 || pages[0].Cursor == "" {
		t.Fatalf("page 1 %+v", pages[0])
	}
	if len(pages[1].Threads) != 1 || pages[1].Threads[0].ID != "9000" || pages[1].Cursor != "" {
		t.Fatalf("page 2 %+v", pages[1])
	}
	raw, err := base64.RawURLEncoding.DecodeString(pages[0].Cursor)
	if err != nil || string(raw) != "2000000:4000" {
		t.Fatalf("cursor %q %v", raw, err)
	}

	if _, err := s.ListThreads(t.Context(), api.ListThreadsParams{Limit: 2, Cursor: "%%%"}); err == nil || err.(*api.Error).Code != api.CodeBadParams {
		t.Fatalf("bad cursor error %v", err)
	}
}

func TestListThreadsNotConnectedWhenEmpty(t *testing.T) {
	s, _ := openTest(t, newFakeClient(nil))
	_, err := s.ListThreads(t.Context(), api.ListThreadsParams{Limit: 5})
	if ae, ok := err.(*api.Error); !ok || ae.Code != api.CodeNotConnected {
		t.Fatalf("error %v", err)
	}
}

func TestListThreadsFetchesMore(t *testing.T) {
	oldDelay := listRoundDelay
	listRoundDelay = time.Millisecond
	t.Cleanup(func() { listRoundDelay = oldDelay })

	fc := newFakeClient(nil)
	calls := 0
	fc.fetchMore = func(_ context.Context, g int64) (*socket.KeyStoreData, *table.LSTable, error) {
		calls++
		if g != 1 {
			t.Fatalf("unexpected sync group %d", g)
		}
		if calls == 1 {
			tbl := &table.LSTable{LSDeleteThenInsertThread: []*table.LSDeleteThenInsertThread{
				{ThreadKey: 6000, ThreadType: table.GROUP_THREAD, ThreadName: "Old", LastActivityTimestampMs: 500_000},
			}}
			return &socket.KeyStoreData{HasMoreBefore: true, MinThreadKey: 6000}, tbl, nil
		}
		return nil, nil, nil
	}
	s, _ := openTest(t, fc)
	if _, err := s.Validate(t.Context()); err != nil {
		t.Fatal(err)
	}
	fc.mu.Lock()
	fc.connected = true
	fc.mu.Unlock()
	s.processTable(threadsTable(), modeInitial)

	res, err := s.ListThreads(t.Context(), api.ListThreadsParams{Limit: 4})
	if err != nil {
		t.Fatal(err)
	}
	if len(res.Threads) != 4 || res.Threads[3].ID != "6000" {
		t.Fatalf("threads %+v", res.Threads)
	}
	if res.Cursor == "" {
		t.Fatal("a full page with fetching not exhausted must keep a cursor")
	}
	res, err = s.ListThreads(t.Context(), api.ListThreadsParams{Limit: 4, Cursor: res.Cursor})
	if err != nil {
		t.Fatal(err)
	}
	if len(res.Threads) != 0 || res.Cursor != "" {
		t.Fatalf("exhausted fetching must end the cursor: %+v", res)
	}
	if calls != 2 {
		t.Fatalf("FetchMoreThreads calls %d", calls)
	}
	fc.mu.Lock()
	posted := len(fc.posted)
	fc.mu.Unlock()
	if posted != 1 {
		t.Fatalf("PostHandlePublishResponse calls %d", posted)
	}
}

func TestThreadsFileRoundTrip(t *testing.T) {
	s, _ := openTest(t, newFakeClient(nil))
	s.processTable(threadsTable(), modeInitial)

	path := filepath.Join(s.dataDir, threadsFileName)
	fi, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if fi.Mode().Perm() != 0o600 {
		t.Fatalf("threads.json mode %v", fi.Mode().Perm())
	}
	if _, err := os.Stat(path + ".tmp"); err == nil {
		t.Fatal("temp file must not remain")
	}

	again := newRegistry(s.dataDir, "111", testMe, nil)
	if got := again.threadID(5000); got != "9000" {
		t.Fatalf("restored mapping %s", got)
	}
	if got := again.threadID(alice); got != "222" {
		t.Fatalf("unmapped key %s", got)
	}
	if err := s.f.Wipe(s.dataDir); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(path); err == nil {
		t.Fatal("Wipe must remove threads.json")
	}
	again.forget()
	if got := again.threadID(5000); got != "5000" {
		t.Fatalf("forget must drop the mapping, got %s", got)
	}
}

func TestRegistryDeleteAndParticipants(t *testing.T) {
	s, _ := openTest(t, newFakeClient(nil))
	s.processTable(threadsTable(), modeInitial)
	s.processTable(&table.LSTable{
		LSRemoveParticipantFromThread: []*table.LSRemoveParticipantFromThread{{ThreadKey: 4000, ParticipantId: bob}},
		LSDeleteThread:                []*table.LSDeleteThread{{ThreadKey: alice}},
	}, modeLive)
	if _, ok := s.threadInfo("222"); ok {
		t.Fatal("deleted thread must be gone")
	}
	snap, _ := s.reg.snapshot("4000")
	if len(snap.Participants) != 2 {
		t.Fatalf("participants %+v", snap.Participants)
	}

	s.processTable(&table.LSTable{
		LSDeleteThread: []*table.LSDeleteThread{{ThreadKey: 4000}},
		LSUpdateOrInsertThread: []*table.LSUpdateOrInsertThread{
			{ThreadKey: 4000, ThreadType: table.GROUP_THREAD, ThreadName: "Crew 2"},
		},
	}, modeLive)
	snap, ok := s.reg.snapshot("4000")
	if !ok || snap.Name == nil || *snap.Name != "Crew 2" {
		t.Fatalf("delete with an active upsert in the same table must keep the thread: %+v", snap)
	}

	s.processTable(&table.LSTable{
		LSRemoveParticipantFromThread: []*table.LSRemoveParticipantFromThread{{ThreadKey: 4000, ParticipantId: testMe}},
	}, modeLive)
	if _, ok := s.threadInfo("4000"); ok {
		t.Fatal("leaving a thread must drop it")
	}
}
