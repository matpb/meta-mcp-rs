package wa

import (
	"bytes"
	"context"
	"errors"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/rs/zerolog"
	"go.mau.fi/whatsmeow/proto/waAdv"
	"go.mau.fi/whatsmeow/proto/waCompanionReg"
	"go.mau.fi/whatsmeow/proto/waWa6"
	"go.mau.fi/whatsmeow/store"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	"google.golang.org/protobuf/proto"
)

var (
	_ Client  = (*Real)(nil)
	_ Factory = (*Session)(nil)
)

func openTemp(t *testing.T, dir string) *Session {
	t.Helper()
	s, err := Open(context.Background(), dir, zerolog.Nop())
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	return s
}

func TestOpenCreatesFilesWithMode0600(t *testing.T) {
	dir := t.TempDir()
	s := openTemp(t, dir)
	defer s.Close()
	for _, name := range []string{"session.db", "sidecar.lock"} {
		fi, err := os.Stat(filepath.Join(dir, name))
		if err != nil {
			t.Fatalf("stat %s: %v", name, err)
		}
		if got := fi.Mode().Perm(); got != 0o600 {
			t.Errorf("%s mode = %o, want 600", name, got)
		}
	}
	for _, suffix := range []string{"-wal", "-shm"} {
		if fi, err := os.Stat(filepath.Join(dir, "session.db"+suffix)); err == nil && fi.Mode().Perm() != 0o600 {
			t.Errorf("session.db%s mode = %o, want 600", suffix, fi.Mode().Perm())
		}
	}
}

func TestSecondOpenIsLockedUntilClose(t *testing.T) {
	dir := t.TempDir()
	s := openTemp(t, dir)
	if _, err := Open(context.Background(), dir, zerolog.Nop()); !errors.Is(err, ErrLocked) {
		t.Fatalf("second Open err = %v, want ErrLocked", err)
	}
	if err := s.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if err := s.Close(); err != nil {
		t.Fatalf("second Close: %v", err)
	}
	s2 := openTemp(t, dir)
	defer s2.Close()
}

func TestNewClientUnpairedDefaults(t *testing.T) {
	s := openTemp(t, t.TempDir())
	defer s.Close()
	c, err := s.NewClient(context.Background(), nil)
	if err != nil {
		t.Fatalf("NewClient: %v", err)
	}
	if c.HasDevice() || c.IsLoggedIn() || c.IsConnected() {
		t.Errorf("HasDevice=%v IsLoggedIn=%v IsConnected=%v, want all false", c.HasDevice(), c.IsLoggedIn(), c.IsConnected())
	}
	self := c.Self()
	if !self.JID.IsEmpty() || !self.LID.IsEmpty() || self.PushName != "" {
		t.Errorf("Self = %+v, want empty", self)
	}
	r, ok := c.(*Real)
	if !ok || r.Raw() == nil {
		t.Fatalf("client is %T, want *Real with a raw client", c)
	}
}

func TestEventHandlerReplaceable(t *testing.T) {
	s := openTemp(t, t.TempDir())
	defer s.Close()
	c, _ := s.NewClient(context.Background(), nil)
	calls := ""
	c.SetEventHandler(func(any) bool { calls += "a"; return true })
	c.SetEventHandler(func(any) bool { calls += "b"; return false })
	r := c.(*Real)
	r.mu.Lock()
	h := r.handler
	r.mu.Unlock()
	if h(nil) {
		t.Error("replaced handler result not propagated")
	}
	if calls != "b" {
		t.Errorf("calls = %q, want b", calls)
	}
}

func TestNewClientLoginOptionsSetDeviceProps(t *testing.T) {
	devicePropsMu.Lock()
	savedProps := proto.Clone(store.DeviceProps).(*waCompanionReg.DeviceProps)
	savedPayload := proto.Clone(store.BaseClientPayload).(*waWa6.ClientPayload)
	devicePropsMu.Unlock()
	t.Cleanup(func() {
		devicePropsMu.Lock()
		store.DeviceProps = savedProps
		store.BaseClientPayload = savedPayload
		devicePropsMu.Unlock()
	})

	s := openTemp(t, t.TempDir())
	defer s.Close()
	if _, err := s.NewClient(context.Background(), &LoginOptions{DeviceName: "meta-mcp", FullHistory: true}); err != nil {
		t.Fatalf("NewClient: %v", err)
	}
	p := store.DeviceProps
	if p.GetOs() != "meta-mcp" {
		t.Errorf("Os = %q", p.GetOs())
	}
	if !p.GetRequireFullSync() {
		t.Error("RequireFullSync false")
	}
	cfg := p.GetHistorySyncConfig()
	if cfg.GetFullSyncDaysLimit() != 3650 || cfg.GetFullSyncSizeMbLimit() != 10240 || cfg.GetStorageQuotaMb() != 10240 {
		t.Errorf("history config = %d/%d/%d", cfg.GetFullSyncDaysLimit(), cfg.GetFullSyncSizeMbLimit(), cfg.GetStorageQuotaMb())
	}
	if p.GetPlatformType() != waCompanionReg.DeviceProps_UNKNOWN {
		t.Errorf("PlatformType = %v", p.GetPlatformType())
	}

	if _, err := s.NewClient(context.Background(), &LoginOptions{DeviceName: "x", FullHistory: false}); err != nil {
		t.Fatalf("NewClient: %v", err)
	}
	if store.DeviceProps.GetRequireFullSync() || store.DeviceProps.GetHistorySyncConfig().FullSyncDaysLimit != nil {
		t.Error("FullHistory=false did not clear full sync settings")
	}
}

func TestOpenAppliesPragmas(t *testing.T) {
	s := openTemp(t, t.TempDir())
	defer s.Close()
	var fk, secure int
	var mode string
	if err := s.db.QueryRow("PRAGMA foreign_keys").Scan(&fk); err != nil || fk != 1 {
		t.Errorf("foreign_keys = %d, err %v", fk, err)
	}
	if err := s.db.QueryRow("PRAGMA secure_delete").Scan(&secure); err != nil || secure != 1 {
		t.Errorf("secure_delete = %d, err %v", secure, err)
	}
	if err := s.db.QueryRow("PRAGMA journal_mode").Scan(&mode); err != nil || mode != "wal" {
		t.Errorf("journal_mode = %q, err %v", mode, err)
	}
}

// pairedDevice stores a linked device with an identity key set to marker, plus a LID mapping and a privacy token.
func pairedDevice(t *testing.T, s *Session, marker []byte) *Real {
	t.Helper()
	ctx := context.Background()
	c, err := s.NewClient(ctx, nil)
	if err != nil {
		t.Fatal(err)
	}
	r := c.(*Real)
	st := r.Raw().Store
	jid := types.NewADJID("15551234567", 0, 7)
	st.ID = &jid
	st.LID = types.NewJID("100000000000001", types.HiddenUserServer)
	st.Platform = "android"
	st.Account = &waAdv.ADVSignedDeviceIdentity{Details: []byte("d"), AccountSignature: make([]byte, 64), AccountSignatureKey: make([]byte, 32), DeviceSignature: make([]byte, 64)}
	copy(st.IdentityKey.Priv[:], marker)
	if err := st.Save(ctx); err != nil {
		t.Fatal(err)
	}
	if err := st.LIDs.PutLIDMapping(ctx, types.NewJID("111222333", types.HiddenUserServer), types.NewJID("14155550100", types.DefaultUserServer)); err != nil {
		t.Fatal(err)
	}
	if err := st.PrivacyTokens.PutPrivacyTokens(ctx, store.PrivacyToken{User: types.NewJID("14155550100", types.DefaultUserServer), Token: []byte("tok"), Timestamp: time.Now()}); err != nil {
		t.Fatal(err)
	}
	// many rows, as a real device has (pre-keys, sessions): freed pages are not zeroed by a plain DELETE
	for i := 0; i < 400; i++ {
		if _, err := s.db.ExecContext(ctx, "INSERT INTO whatsmeow_pre_keys (jid, key_id, key, uploaded) VALUES (?, ?, ?, 1)", jid.String(), i+10, marker); err != nil {
			t.Fatalf("insert prekey: %v", err)
		}
	}
	return r
}

func countRows(t *testing.T, s *Session, table string) int {
	t.Helper()
	var n int
	if err := s.db.QueryRowContext(context.Background(), "SELECT COUNT(*) FROM "+table).Scan(&n); err != nil {
		t.Fatalf("count %s: %v", table, err)
	}
	return n
}

// Review L2: after DeleteDevice (what logout does) session.db kept the LID map, the privacy tokens and key bytes in freed pages.
func TestReviewDeleteDeviceLeavesResidue(t *testing.T) {
	dir := t.TempDir()
	s := openTemp(t, dir)
	marker := bytes.Repeat([]byte{0xAB}, 32)
	r := pairedDevice(t, s, marker)
	ctx := context.Background()
	_, _ = s.db.ExecContext(ctx, "PRAGMA wal_checkpoint(TRUNCATE)")
	pre, _ := os.ReadFile(filepath.Join(dir, "session.db"))
	if !bytes.Contains(pre, marker) {
		t.Fatal("setup: the key bytes must be in session.db before the delete")
	}
	if err := r.DeleteDevice(ctx); err != nil {
		t.Fatal(err)
	}
	if devs, lids, toks := countRows(t, s, "whatsmeow_device"), countRows(t, s, "whatsmeow_lid_map"), countRows(t, s, "whatsmeow_privacy_tokens"); devs != 0 || lids != 0 || toks != 0 {
		t.Errorf("after DeleteDevice: devices=%d lid_map=%d privacy_tokens=%d", devs, lids, toks)
	}
	// flush the WAL into the main file and search for the private key bytes
	_, _ = s.db.ExecContext(ctx, "PRAGMA wal_checkpoint(TRUNCATE)")
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"session.db", "session.db-wal"} {
		raw, _ := os.ReadFile(filepath.Join(dir, name))
		if bytes.Contains(raw, marker) {
			t.Errorf("the identity private key bytes are still in %s after the device row was deleted", name)
		}
	}
}

func TestScrubAfterAnExternalDeleteAndOnALiveDevice(t *testing.T) {
	dir := t.TempDir()
	s := openTemp(t, dir)
	defer s.Close()
	marker := bytes.Repeat([]byte{0xCD}, 32)
	r := pairedDevice(t, s, marker)
	ctx := context.Background()
	if err := s.Scrub(ctx); err != nil {
		t.Fatal(err)
	}
	if countRows(t, s, "whatsmeow_device") != 1 || countRows(t, s, "whatsmeow_pre_keys") != 400 {
		t.Fatal("scrub must leave the device and its keys alone")
	}
	if countRows(t, s, "whatsmeow_lid_map") != 0 || countRows(t, s, "whatsmeow_privacy_tokens") != 0 {
		t.Fatal("scrub clears the caches no device row owns")
	}
	// the library deletes the device itself on LoggedOut; the factory scrub then removes what is left
	if err := r.Raw().Store.Delete(ctx); err != nil {
		t.Fatal(err)
	}
	if err := s.Scrub(ctx); err != nil {
		t.Fatal(err)
	}
	raw, _ := os.ReadFile(filepath.Join(dir, "session.db"))
	if bytes.Contains(raw, marker) {
		t.Error("key bytes survive a scrub after the device was deleted")
	}
	for _, suffix := range []string{"", "-wal", "-shm"} {
		if fi, err := os.Stat(filepath.Join(dir, "session.db"+suffix)); err == nil && fi.Mode().Perm() != 0o600 {
			t.Errorf("session.db%s mode %o after the scrub", suffix, fi.Mode().Perm())
		}
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	if err := s.Scrub(ctx); err == nil {
		t.Error("a scrub on a closed session must fail")
	}
}

// Review W4: a panic in the handler is reported to whatsmeow as a failed handler, which leaves the event unacknowledged.
func TestReviewPanicInHandlerIsAckedByLibrary(t *testing.T) {
	s := openTemp(t, t.TempDir())
	defer s.Close()
	c, err := s.NewClient(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	r := c.(*Real)
	r.SetEventHandler(func(evt any) bool { panic("boom in handler") })
	if failed := r.Raw().DangerousInternals().DispatchEvent(&events.Message{}); !failed {
		t.Errorf("whatsmeow treats a panicking handler as success (handlerFailed=false), so the message would be acked")
	}
	r.SetEventHandler(func(evt any) bool { return true })
	if failed := r.Raw().DangerousInternals().DispatchEvent(&events.Message{}); failed {
		t.Error("a healthy handler must still succeed after a panic")
	}
}
