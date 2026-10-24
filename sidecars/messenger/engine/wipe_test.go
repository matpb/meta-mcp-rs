package engine

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/rs/zerolog"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
	"go.mau.fi/whatsmeow/proto/waAdv"
	"go.mau.fi/whatsmeow/types"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
	"github.com/matpb/meta-mcp-rs/sidecars/messenger/e2ee"
)

func touch(t *testing.T, path string) {
	t.Helper()
	if err := os.WriteFile(path, []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
}

func exists(path string) bool {
	_, err := os.Lstat(path)
	return err == nil
}

// M3: the factory wipes without any session; the process lock file and foreign files stay.
func TestFactoryWipeNeedsNoSession(t *testing.T) {
	dir := t.TempDir()
	wiped := []string{"e2ee.db", "e2ee.db-wal", "e2ee.db-shm", "e2ee.db-journal", threadsFileName, threadsFileName + ".tmp"}
	kept := []string{"sidecar.lock", "unrelated.txt"}
	for _, n := range append(append([]string{}, wiped...), kept...) {
		touch(t, filepath.Join(dir, n))
	}
	f := NewFactory(zerolog.Nop())
	if err := f.Wipe(dir); err != nil {
		t.Fatal(err)
	}
	for _, n := range wiped {
		if exists(filepath.Join(dir, n)) {
			t.Errorf("%s survived the wipe", n)
		}
	}
	for _, n := range kept {
		if !exists(filepath.Join(dir, n)) {
			t.Errorf("%s must not be touched", n)
		}
	}
	if err := f.Wipe(dir); err != nil {
		t.Fatalf("a second wipe finds nothing and must succeed: %v", err)
	}
}

func TestFactoryWipeFailureIsAnIOError(t *testing.T) {
	dir := t.TempDir()
	touch(t, filepath.Join(dir, "e2ee.db"))
	// a non-empty directory under the name of the thread map cannot be removed
	if err := os.MkdirAll(filepath.Join(dir, threadsFileName, "x"), 0o700); err != nil {
		t.Fatal(err)
	}
	err := NewFactory(zerolog.Nop()).Wipe(dir)
	ae := wantCode(t, err, api.CodeIO)
	if ae.Msg != "failed to delete local session data" || strings.Contains(ae.Msg, dir) {
		t.Fatalf("message %q", ae.Msg)
	}
	if exists(filepath.Join(dir, "e2ee.db")) {
		t.Fatal("the other files are still wiped when one fails")
	}
}

func TestLogoutReportsAFailedWipeButClearsTheSession(t *testing.T) {
	s, _ := openTest(t, newFakeClient(nil))
	if err := os.MkdirAll(filepath.Join(s.dataDir, threadsFileName, "x"), 0o700); err != nil {
		t.Fatal(err)
	}
	wantCode(t, s.Logout(t.Context()), api.CodeIO)
	if st := s.State(); len(st.Cookies) != 0 || st.E2EERegistered {
		t.Fatalf("state after a failed wipe %+v", st)
	}
	if err := s.usable(); err == nil {
		t.Fatal("the session must refuse work after logout")
	}
}

func registerForeignDevice(t *testing.T, dir, fbid string) {
	t.Helper()
	st, err := e2ee.Open(t.Context(), dir, zerolog.Nop())
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()
	dev, err := st.Container().GetFirstDevice(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	id := types.JID{User: fbid, Device: 7, Server: types.MessengerServer}
	dev.ID = &id
	dev.Account = &waAdv.ADVSignedDeviceIdentity{
		Details: []byte{1}, AccountSignature: make([]byte, 64), AccountSignatureKey: make([]byte, 32), DeviceSignature: make([]byte, 64),
	}
	if err := dev.Save(t.Context()); err != nil {
		t.Fatal(err)
	}
}

// M3: an e2ee.db that belongs to another account is wiped instead of reused.
func TestEnsureStoreWipesTheDeviceOfAnotherAccount(t *testing.T) {
	dir := t.TempDir()
	registerForeignDevice(t, dir, "999")
	s, rec := openIn(t, newFakeClient(nil), dir)
	s.e2eeRegistered = true
	s.watch.reset(s.State())
	st, err := s.ensureStore(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	dev, err := st.Container().GetFirstDevice(t.Context())
	if err != nil || dev.ID != nil {
		t.Fatalf("the device of account 999 was reused: %v %v", dev, err)
	}
	if s.State().E2EERegistered {
		t.Fatal("a wiped device means the account is not registered")
	}
	evs := rec.named(api.EvStateUpdated)
	if len(evs) != 1 || evs[0].data.(api.StateUpdatedEvent).State.E2EERegistered {
		t.Fatalf("want one state_updated with the flag cleared, got %d", len(evs))
	}
}

func TestEnsureStoreKeepsTheDeviceOfTheSameAccount(t *testing.T) {
	dir := t.TempDir()
	registerForeignDevice(t, dir, "111")
	s, _ := openIn(t, newFakeClient(nil), dir)
	st, err := s.ensureStore(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	if dev, err := st.Container().GetFirstDevice(t.Context()); err != nil || dev.ID == nil || dev.ID.User != "111" {
		t.Fatalf("the device of the same account must survive: %v %v", dev, err)
	}
}

func writeThreadsFile(t *testing.T, dir, body string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(dir, threadsFileName), []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
}

func TestThreadMapOfAnotherAccountIsIgnored(t *testing.T) {
	dir := t.TempDir()
	writeThreadsFile(t, dir, `{"version":1,"fbid":"999","hybrid":{"5000":{"key":9000,"type":0}}}`)
	if got := newRegistry(dir, "111", testMe, nil).threadID(5000); got != "5000" {
		t.Fatalf("a map of account 999 was applied to 111: %s", got)
	}
	writeThreadsFile(t, dir, `{"version":1,"fbid":"111","hybrid":{"5000":{"key":9000,"type":0}}}`)
	if got := newRegistry(dir, "111", testMe, nil).threadID(5000); got != "9000" {
		t.Fatalf("the map of the same account must load: %s", got)
	}
	writeThreadsFile(t, dir, `{"version":1,"hybrid":{"5000":{"key":9000,"type":0}}}`)
	if got := newRegistry(dir, "111", testMe, nil).threadID(5000); got != "9000" {
		t.Fatalf("a map from before accounts were recorded is adopted: %s", got)
	}
}

func TestThreadMapRecordsItsAccount(t *testing.T) {
	s, _ := openTest(t, newFakeClient(nil))
	s.processTable(threadsTable(), modeInitial)
	b, err := os.ReadFile(filepath.Join(s.dataDir, threadsFileName))
	if err != nil {
		t.Fatal(err)
	}
	var f threadsFile
	if err := json.Unmarshal(b, &f); err != nil || f.FBID != "111" {
		t.Fatalf("threads.json %s (%v)", b, err)
	}
}

// Shared decision: history events carry at most 200 messages and about 4 MiB of JSON.
func TestSplitHistoryCaps(t *testing.T) {
	msg := func(i int, text string) api.Message {
		return api.Message{ID: fmt.Sprintf("fb:mid.$%04d", i), ThreadID: "222", Text: &text, Attachments: []api.Attachment{}, Reactions: []api.Reaction{}}
	}
	small := make([]api.Message, 450)
	for i := range small {
		small[i] = msg(i, "x")
	}
	batches := splitHistory(small)
	if len(batches) != 3 || len(batches[0]) != 200 || len(batches[1]) != 200 || len(batches[2]) != 50 {
		t.Fatalf("batch sizes %d %d %d (%d batches)", len(batches[0]), len(batches[1]), len(batches[len(batches)-1]), len(batches))
	}
	if batches[0][0].ID != small[0].ID || batches[2][49].ID != small[449].ID {
		t.Fatal("order must be kept")
	}

	big := strings.Repeat("a", 1500*1024)
	heavy := []api.Message{msg(1, big), msg(2, big), msg(3, big), msg(4, big), msg(5, big)}
	batches = splitHistory(heavy)
	if len(batches) != 3 || len(batches[0]) != 2 || len(batches[1]) != 2 || len(batches[2]) != 1 {
		t.Fatalf("heavy messages split into %d batches", len(batches))
	}
	for i, b := range batches {
		raw, _ := json.Marshal(b)
		if len(raw) > maxHistoryBatchBytes {
			t.Fatalf("batch %d is %d bytes", i, len(raw))
		}
	}

	huge := msg(1, strings.Repeat("b", 5<<20))
	batches = splitHistory([]api.Message{msg(0, "x"), huge, msg(2, "y")})
	if len(batches) != 3 || len(batches[1]) != 1 || batches[1][0].ID != huge.ID {
		t.Fatalf("a message over the byte cap travels alone: %d batches", len(batches))
	}
	if splitHistory(nil) != nil {
		t.Fatal("nothing to send, nothing to split")
	}
}

func TestHistoryEventsAreBatchedAndCompleteComesLast(t *testing.T) {
	s, rec := openTest(t, newFakeClient(nil))
	tbl := &table.LSTable{}
	for i := 1; i <= 450; i++ {
		tbl.LSUpsertMessage = append(tbl.LSUpsertMessage, &table.LSUpsertMessage{
			MessageId: fmt.Sprintf("mid.$%04d", i), ThreadKey: histThread, TimestampMs: int64(i) * 1000, SenderId: alice, Text: "m",
		})
	}
	tbl.LSInsertNewMessageRange = []*table.LSInsertNewMessageRange{{
		ThreadKey: histThread, MinTimestampMs: 1000, MaxTimestampMs: 450_000, MinMessageId: "mid.$0001", MaxMessageId: "mid.$0450", HasMoreBefore: false,
	}}
	s.processTable(tbl, modeInitial)
	evs := rec.named(api.EvHistory)
	if len(evs) != 3 {
		t.Fatalf("want 3 history events, got %d", len(evs))
	}
	var total int
	for i, e := range evs {
		h := e.data.(api.HistoryEvent)
		total += len(h.Messages)
		if len(h.Messages) > maxHistoryBatch || h.ThreadID != "700" {
			t.Fatalf("event %d: %d messages for thread %s", i, len(h.Messages), h.ThreadID)
		}
		if want := i == len(evs)-1; h.Complete != want {
			t.Fatalf("event %d: complete=%v, want %v", i, h.Complete, want)
		}
	}
	if total != 450 {
		t.Fatalf("%d messages emitted in total", total)
	}
}
