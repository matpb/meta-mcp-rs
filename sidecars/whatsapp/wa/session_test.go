package wa

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/rs/zerolog"
	"go.mau.fi/whatsmeow/proto/waCompanionReg"
	"go.mau.fi/whatsmeow/proto/waWa6"
	"go.mau.fi/whatsmeow/store"
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
	var fk int
	var mode string
	if err := s.db.QueryRow("PRAGMA foreign_keys").Scan(&fk); err != nil || fk != 1 {
		t.Errorf("foreign_keys = %d, err %v", fk, err)
	}
	if err := s.db.QueryRow("PRAGMA journal_mode").Scan(&mode); err != nil || mode != "wal" {
		t.Errorf("journal_mode = %q, err %v", mode, err)
	}
}
