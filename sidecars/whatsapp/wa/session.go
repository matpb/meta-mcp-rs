package wa

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"sync"
	"syscall"
	"time"

	"github.com/rs/zerolog"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waCompanionReg"
	"go.mau.fi/whatsmeow/store"
	"go.mau.fi/whatsmeow/store/sqlstore"
	waLog "go.mau.fi/whatsmeow/util/log"
	"google.golang.org/protobuf/proto"
	_ "modernc.org/sqlite"
)

// ErrLocked means another process holds the data_dir lock.
var ErrLocked = errors.New("wa: data dir is locked by another process")

const (
	lockFileName   = "sidecar.lock"
	sessionDBName  = "session.db"
	versionTimeout = 5 * time.Second
)

// devicePropsMu guards the package-level store.DeviceProps while NewClient mutates it.
var devicePropsMu sync.Mutex

// Session owns session.db and the data_dir lock; it implements Factory.
type Session struct {
	log       zerolog.Logger
	db        *sql.DB
	container *sqlstore.Container
	lock      *os.File
	dbPath    string

	versionOnce sync.Once
	closeOnce   sync.Once
	closeErr    error
}

var _ Factory = (*Session)(nil)

// Open locks dataDir, opens session.db and upgrades the whatsmeow schema.
func Open(ctx context.Context, dataDir string, log zerolog.Logger) (*Session, error) {
	lock, err := os.OpenFile(filepath.Join(dataDir, lockFileName), os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return nil, fmt.Errorf("wa: open lock file: %w", err)
	}
	if err := syscall.Flock(int(lock.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		_ = lock.Close()
		if errors.Is(err, syscall.EWOULDBLOCK) {
			return nil, ErrLocked
		}
		return nil, fmt.Errorf("wa: lock data dir: %w", err)
	}
	s := &Session{log: log, lock: lock, dbPath: filepath.Join(dataDir, sessionDBName)}
	if err := s.openDB(ctx); err != nil {
		s.releaseLock()
		return nil, err
	}
	return s, nil
}

func (s *Session) openDB(ctx context.Context) error {
	u := url.URL{Scheme: "file", Path: s.dbPath, RawQuery: "_pragma=foreign_keys(1)&_pragma=busy_timeout(5000)&_pragma=journal_mode(WAL)"}
	db, err := sql.Open("sqlite", u.String())
	if err != nil {
		return fmt.Errorf("wa: open session db: %w", err)
	}
	wmLog := waLog.Zerolog(s.log.With().Str("mod", "whatsmeow").Logger())
	container := sqlstore.NewWithDB(db, "sqlite3", wmLog)
	if err := container.Upgrade(ctx); err != nil {
		_ = db.Close()
		return fmt.Errorf("wa: upgrade session db: %w", err)
	}
	s.db, s.container = db, container
	s.chmodDB()
	return nil
}

// chmodDB tightens session.db and any WAL sidecar files to 0600.
func (s *Session) chmodDB() {
	for _, suffix := range []string{"", "-wal", "-shm"} {
		p := s.dbPath + suffix
		if _, err := os.Stat(p); err == nil {
			_ = os.Chmod(p, 0o600)
		}
	}
}

func (s *Session) releaseLock() {
	_ = syscall.Flock(int(s.lock.Fd()), syscall.LOCK_UN)
	_ = s.lock.Close()
}

// NewClient builds a client on the stored device, or a fresh unpaired one; it never touches the network.
func (s *Session) NewClient(ctx context.Context, opts *LoginOptions) (Client, error) {
	dev, err := s.container.GetFirstDevice(ctx)
	if err != nil {
		return nil, fmt.Errorf("wa: load device: %w", err)
	}
	if opts != nil && dev.ID == nil {
		applyLoginOptions(opts)
	}
	cli := whatsmeow.NewClient(dev, waLog.Zerolog(s.log.With().Str("mod", "client").Logger()))
	cli.SynchronousAck = true
	cli.EnableDecryptedEventBuffer = true
	cli.AutomaticMessageRerequestFromPhone = true
	cli.EnableAutoReconnect = true
	cli.InitialAutoReconnect = true
	s.chmodDB()
	return newReal(cli), nil
}

func applyLoginOptions(opts *LoginOptions) {
	devicePropsMu.Lock()
	defer devicePropsMu.Unlock()
	store.SetOSInfo(opts.DeviceName, [3]uint32{0, 1, 0})
	store.DeviceProps.PlatformType = waCompanionReg.DeviceProps_UNKNOWN.Enum()
	store.DeviceProps.RequireFullSync = proto.Bool(opts.FullHistory)
	cfg := store.DeviceProps.HistorySyncConfig
	if opts.FullHistory {
		cfg.FullSyncDaysLimit = proto.Uint32(3650)
		cfg.FullSyncSizeMbLimit = proto.Uint32(10240)
		cfg.StorageQuotaMb = proto.Uint32(10240)
	} else {
		cfg.FullSyncDaysLimit = nil
		cfg.FullSyncSizeMbLimit = nil
	}
}

// RefreshVersion fetches the current WhatsApp web version once per process; failures are only logged.
func (s *Session) RefreshVersion(ctx context.Context) {
	s.versionOnce.Do(func() {
		ctx, cancel := context.WithTimeout(ctx, versionTimeout)
		defer cancel()
		ver, err := whatsmeow.GetLatestVersion(ctx, &http.Client{Timeout: versionTimeout})
		if err != nil || ver == nil {
			s.log.Info().Msg("wa version refresh failed, keeping built-in version")
			return
		}
		store.SetWAVersion(*ver)
	})
}

// Close closes session.db and releases the lock; it disconnects nothing.
func (s *Session) Close() error {
	s.closeOnce.Do(func() {
		s.chmodDB()
		s.closeErr = s.db.Close()
		s.releaseLock()
	})
	return s.closeErr
}
