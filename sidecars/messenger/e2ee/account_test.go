package e2ee

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/rs/zerolog"
	"go.mau.fi/whatsmeow/proto/waAdv"
	"go.mau.fi/whatsmeow/types"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

// registerDevice stores a registered device for fbid in s, as RegisterE2EE plus Save would.
func registerDevice(t *testing.T, s *Store, fbid string) {
	t.Helper()
	ctx := context.Background()
	dev, err := s.Container().GetFirstDevice(ctx)
	if err != nil {
		t.Fatal(err)
	}
	jid := types.JID{User: fbid, Device: 7, Server: types.MessengerServer}
	dev.ID = &jid
	dev.Account = &waAdv.ADVSignedDeviceIdentity{
		Details: []byte{1}, AccountSignature: make([]byte, 64), AccountSignatureKey: make([]byte, 32), DeviceSignature: make([]byte, 64),
	}
	if err := dev.Save(ctx); err != nil {
		t.Fatalf("save device: %v", err)
	}
}

func deviceUser(t *testing.T, s *Store) string {
	t.Helper()
	dev, err := s.Container().GetFirstDevice(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if dev.ID == nil {
		return ""
	}
	return dev.ID.User
}

func openFor(t *testing.T, dir, fbid string) (*Store, bool) {
	t.Helper()
	s, wiped, err := OpenFor(context.Background(), dir, fbid, zerolog.Nop())
	if err != nil {
		t.Fatalf("OpenFor(%s): %v", fbid, err)
	}
	return s, wiped
}

func TestOpenForBindsAFreshDatabaseAndKeepsItForTheSameAccount(t *testing.T) {
	dir := t.TempDir()
	s, wiped := openFor(t, dir, "111")
	if wiped {
		t.Fatal("a fresh database has nothing to wipe")
	}
	registerDevice(t, s, "111")
	put(t, s, mkMsg("t1", 1), nil)
	_ = s.Close()

	s, wiped = openFor(t, dir, "111")
	defer s.Close()
	if wiped {
		t.Fatal("the same account must keep its data")
	}
	if got := deviceUser(t, s); got != "111" {
		t.Fatalf("device lost: %q", got)
	}
	if _, found, _ := s.Get(context.Background(), mkMsg("t1", 1).ID); !found {
		t.Fatal("cached message lost")
	}
}

func TestOpenForWipesTheDatabaseOfAnotherAccount(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	s, _ := openFor(t, dir, "111")
	registerDevice(t, s, "111")
	put(t, s, mkMsg("t1", 1), []api.E2EEMedia{{AttachmentID: "a1", MediaType: "x", Integral: []byte{1}, Mime: "image/png", Size: 3}})
	if err := s.PutFBAttachments(ctx, []FBAttachments{{MessageID: "fb:mid.$1", ThreadID: "t1", TSMS: 5, Attachments: []api.Attachment{{ID: "0", Kind: api.AttImage}}}}); err != nil {
		t.Fatal(err)
	}
	_ = s.Close()

	s, wiped := openFor(t, dir, "222")
	defer s.Close()
	if !wiped {
		t.Fatal("data of account 111 must be wiped for account 222")
	}
	if got := deviceUser(t, s); got != "" {
		t.Fatalf("the device of another account survived: %q", got)
	}
	if _, found, _ := s.Get(ctx, mkMsg("t1", 1).ID); found {
		t.Fatal("cached plaintext of another account survived")
	}
	if _, found, _ := s.Media(ctx, mkMsg("t1", 1).ID, "a1"); found {
		t.Fatal("media record of another account survived")
	}
	if _, found, _ := s.FBAttachments(ctx, "fb:mid.$1"); found {
		t.Fatal("attachment index of another account survived")
	}
	var bound string
	if err := s.db.QueryRow(`SELECT fbid FROM sidecar_account WHERE id = 1`).Scan(&bound); err != nil || bound != "222" {
		t.Fatalf("bound account %q err %v", bound, err)
	}
}

// The recorded account decides even when no device was ever registered, so cached plaintext cannot cross accounts.
func TestOpenForWipesAnUnregisteredDatabaseBoundToAnotherAccount(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	s, _ := openFor(t, dir, "111")
	put(t, s, mkMsg("t1", 1), nil)
	_ = s.Close()

	s, wiped := openFor(t, dir, "222")
	defer s.Close()
	if !wiped {
		t.Fatal("a database bound to account 111 must be wiped for account 222")
	}
	if _, found, _ := s.Get(ctx, mkMsg("t1", 1).ID); found {
		t.Fatal("cached plaintext of another account survived")
	}
	var bound string
	if err := s.db.QueryRow(`SELECT fbid FROM sidecar_account WHERE id = 1`).Scan(&bound); err != nil || bound != "222" {
		t.Fatalf("bound account %q err %v", bound, err)
	}
}

// A database written before accounts were recorded is judged by the JID of its device.
func TestOpenForJudgesAnUnboundDatabaseByItsDevice(t *testing.T) {
	dir := t.TempDir()
	s, _ := openFor(t, dir, "111")
	registerDevice(t, s, "111")
	if _, err := s.db.Exec(`DELETE FROM sidecar_account`); err != nil {
		t.Fatal(err)
	}
	_ = s.Close()

	s, wiped := openFor(t, dir, "111")
	if wiped || deviceUser(t, s) != "111" {
		t.Fatalf("a legacy database of the same account must be adopted: wiped=%v", wiped)
	}
	_, err := s.db.Exec(`DELETE FROM sidecar_account`)
	if err != nil {
		t.Fatal(err)
	}
	_ = s.Close()

	s, wiped = openFor(t, dir, "333")
	defer s.Close()
	if !wiped || deviceUser(t, s) != "" {
		t.Fatalf("a legacy database with a foreign device must be wiped: wiped=%v", wiped)
	}
}

func TestOpenForRefusesAnEmptyAccount(t *testing.T) {
	if _, _, err := OpenFor(context.Background(), t.TempDir(), "", zerolog.Nop()); err == nil {
		t.Fatal("an unbound open needs an account")
	}
}

func TestOpenForWipeFailureIsAnIOError(t *testing.T) {
	dir := t.TempDir()
	s, _ := openFor(t, dir, "111")
	registerDevice(t, s, "111")
	_ = s.Close()
	// a non-empty directory squats on the journal name, so Wipe cannot remove it
	j := filepath.Join(dir, DBName+"-journal")
	if err := os.MkdirAll(filepath.Join(j, "x"), 0o700); err != nil {
		t.Fatal(err)
	}
	_, _, err := OpenFor(context.Background(), dir, "222", zerolog.Nop())
	var ae *api.Error
	if !errors.As(err, &ae) || ae.Code != api.CodeIO {
		t.Fatalf("want an io error, got %v", err)
	}
}

// A v1 database (no account table, no attachment index) upgrades in place and keeps its messages.
func TestSchemaUpgradeFromV1KeepsMessages(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	s := openStore(t, dir)
	put(t, s, mkMsg("t1", 1), nil)
	for _, q := range []string{`DROP TABLE sidecar_account`, `DROP TABLE sidecar_fb_attachments`, `UPDATE sidecar_version SET version = 1`} {
		if _, err := s.db.Exec(q); err != nil {
			t.Fatal(err)
		}
	}
	_ = s.Close()

	s = openStore(t, dir)
	defer s.Close()
	var version int
	if err := s.db.QueryRow(`SELECT version FROM sidecar_version`).Scan(&version); err != nil || version != schemaVersion {
		t.Fatalf("version %d err %v", version, err)
	}
	if _, found, _ := s.Get(ctx, mkMsg("t1", 1).ID); !found {
		t.Fatal("message lost in the upgrade")
	}
	if err := s.bind(ctx, "111"); err != nil {
		t.Fatalf("account table missing after the upgrade: %v", err)
	}
	if err := s.PutFBAttachments(ctx, []FBAttachments{{MessageID: "fb:m", ThreadID: "t", TSMS: 1, Attachments: []api.Attachment{{ID: "0"}}}}); err != nil {
		t.Fatalf("attachment table missing after the upgrade: %v", err)
	}
}

func TestSchemaNewerThanThisBuildIsRefused(t *testing.T) {
	dir := t.TempDir()
	s := openStore(t, dir)
	if _, err := s.db.Exec(`UPDATE sidecar_version SET version = ?`, schemaVersion+1); err != nil {
		t.Fatal(err)
	}
	_ = s.Close()
	if _, err := Open(context.Background(), dir, zerolog.Nop()); err == nil {
		t.Fatal("a database from a newer build must not be opened")
	}
}
