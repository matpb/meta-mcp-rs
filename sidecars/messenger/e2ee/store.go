// Package e2ee owns the on-disk E2EE state: the whatsmeow device store, the encrypted-message cache
// and the attachment index of unencrypted messages.
package e2ee

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"

	"github.com/rs/zerolog"
	"go.mau.fi/whatsmeow/store/sqlstore"
	waLog "go.mau.fi/whatsmeow/util/log"
	_ "modernc.org/sqlite"

	"github.com/matpb/meta-mcp-rs/sidecars/messenger/api"
)

// DBName is the file under the data dir that holds the device store and the message cache.
const DBName = "e2ee.db"

// Tunables; tests lower them.
var (
	retainCap  = 2000
	pruneSlack = 100
	ringCap    = 200
)

const schemaVersion = 2

// ErrForeignAccount means the database holds data of another Facebook account.
var ErrForeignAccount = errors.New("e2ee store: database belongs to another account")

// Store is the E2EE database handle plus the message cache. Safe for concurrent use.
type Store struct {
	db        *sql.DB
	container *sqlstore.Container

	mu      sync.Mutex
	threads map[string]*threadState
}

func ioErr(op string) *api.Error {
	return &api.Error{Code: api.CodeIO, Msg: "e2ee store: " + op + " failed"}
}

// OpenFor opens the database for one account. Data of another account is wiped first and
// wiped reports it; the device store and the caches then start empty.
func OpenFor(ctx context.Context, dataDir, fbid string, log zerolog.Logger) (st *Store, wiped bool, err error) {
	if st, err = Open(ctx, dataDir, log); err != nil {
		return nil, false, err
	}
	err = st.bind(ctx, fbid)
	if err == nil {
		return st, false, nil
	}
	_ = st.Close()
	if !errors.Is(err, ErrForeignAccount) {
		return nil, false, err
	}
	if werr := Wipe(dataDir); werr != nil {
		return nil, false, ioErr("wipe")
	}
	if st, err = Open(ctx, dataDir, log); err != nil {
		return nil, false, err
	}
	if err = st.bind(ctx, fbid); err != nil {
		_ = st.Close()
		return nil, false, err
	}
	return st, true, nil
}

// bind records fbid on a database that has none and refuses one bound to, or holding a device of, another account.
func (s *Store) bind(ctx context.Context, fbid string) error {
	if fbid == "" {
		return ioErr("account")
	}
	var stored string
	err := s.db.QueryRowContext(ctx, `SELECT fbid FROM sidecar_account WHERE id = 1`).Scan(&stored)
	switch {
	case errors.Is(err, sql.ErrNoRows):
	case err != nil:
		return ioErr("account")
	case stored != fbid:
		return ErrForeignAccount
	}
	devs, err := s.container.GetAllDevices(ctx)
	if err != nil {
		return ioErr("account")
	}
	for _, d := range devs {
		if d.ID != nil && d.ID.User != fbid {
			return ErrForeignAccount
		}
	}
	if stored == "" {
		if _, err := s.db.ExecContext(ctx, `INSERT INTO sidecar_account (id, fbid) VALUES (1, ?)`, fbid); err != nil {
			return ioErr("account")
		}
	}
	return nil
}

// Open creates or opens dataDir/e2ee.db (0600), runs whatsmeow and cache upgrades.
func Open(ctx context.Context, dataDir string, log zerolog.Logger) (*Store, error) {
	if err := os.MkdirAll(dataDir, 0o700); err != nil {
		return nil, ioErr("create data dir")
	}
	path := filepath.Join(dataDir, DBName)
	f, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return nil, ioErr("create db file")
	}
	_ = f.Close()
	if err := os.Chmod(path, 0o600); err != nil {
		return nil, ioErr("chmod db file")
	}

	db, err := sql.Open("sqlite", dsn(path))
	if err != nil {
		return nil, ioErr("open")
	}
	db.SetMaxOpenConns(1)

	container := sqlstore.NewWithDB(db, "sqlite3", waLog.Zerolog(log))
	if err := container.Upgrade(ctx); err != nil {
		_ = db.Close()
		return nil, ioErr("device store upgrade")
	}
	if err := upgradeCache(ctx, db); err != nil {
		_ = db.Close()
		return nil, err
	}
	for _, suffix := range []string{"-wal", "-shm"} {
		if _, err := os.Stat(path + suffix); err == nil {
			_ = os.Chmod(path+suffix, 0o600)
		}
	}
	return &Store{db: db, container: container, threads: map[string]*threadState{}}, nil
}

// uriPath turns a native path into the slash form SQLite URIs expect (/C:/dir/file.db on Windows).
func uriPath(p string) string {
	p = filepath.ToSlash(p)
	if !strings.HasPrefix(p, "/") {
		p = "/" + p
	}
	return p
}

var dsnEscaper = strings.NewReplacer("%", "%25", "?", "%3f", "#", "%23")

func dsn(path string) string {
	return "file:" + dsnEscaper.Replace(uriPath(path)) +
		"?_pragma=foreign_keys(1)&_pragma=journal_mode(WAL)&_pragma=busy_timeout(5000)" +
		"&_pragma=synchronous(FULL)&_txlock=immediate"
}

func upgradeCache(ctx context.Context, db *sql.DB) error {
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return ioErr("cache upgrade")
	}
	defer func() { _ = tx.Rollback() }()
	if _, err := tx.ExecContext(ctx, `CREATE TABLE IF NOT EXISTS sidecar_version (version INTEGER NOT NULL)`); err != nil {
		return ioErr("cache upgrade")
	}
	var version int
	err = tx.QueryRowContext(ctx, `SELECT version FROM sidecar_version LIMIT 1`).Scan(&version)
	if errors.Is(err, sql.ErrNoRows) {
		version = 0
	} else if err != nil {
		return ioErr("cache upgrade")
	}
	if version > schemaVersion {
		return ioErr("cache schema is newer than this build")
	}
	if version < 1 {
		stmts := []string{
			`CREATE TABLE sidecar_e2ee_messages (
				id TEXT PRIMARY KEY,
				thread_id TEXT NOT NULL,
				ts_ms INTEGER NOT NULL,
				json BLOB NOT NULL
			)`,
			`CREATE INDEX sidecar_e2ee_messages_thread ON sidecar_e2ee_messages (thread_id, ts_ms, id)`,
			`CREATE TABLE sidecar_e2ee_media (
				message_id TEXT NOT NULL,
				attachment_id TEXT NOT NULL,
				media_type TEXT NOT NULL,
				integral BLOB NOT NULL,
				mime TEXT NOT NULL,
				size INTEGER NOT NULL,
				PRIMARY KEY (message_id, attachment_id)
			)`,
		}
		for _, s := range stmts {
			if _, err := tx.ExecContext(ctx, s); err != nil {
				return ioErr("cache upgrade")
			}
		}
	}
	if version < 2 {
		stmts := []string{
			`CREATE TABLE sidecar_account (
				id INTEGER PRIMARY KEY CHECK (id = 1),
				fbid TEXT NOT NULL
			)`,
			`CREATE TABLE sidecar_fb_attachments (
				message_id TEXT PRIMARY KEY,
				thread_id TEXT NOT NULL,
				ts_ms INTEGER NOT NULL,
				attachments BLOB NOT NULL
			)`,
			`CREATE INDEX sidecar_fb_attachments_ts ON sidecar_fb_attachments (ts_ms, message_id)`,
		}
		for _, s := range stmts {
			if _, err := tx.ExecContext(ctx, s); err != nil {
				return ioErr("cache upgrade")
			}
		}
	}
	if version != schemaVersion {
		if _, err := tx.ExecContext(ctx, `DELETE FROM sidecar_version`); err != nil {
			return ioErr("cache upgrade")
		}
		if _, err := tx.ExecContext(ctx, `INSERT INTO sidecar_version (version) VALUES (?)`, schemaVersion); err != nil {
			return ioErr("cache upgrade")
		}
	}
	if err := tx.Commit(); err != nil {
		return ioErr("cache upgrade")
	}
	return nil
}

// Container is the whatsmeow device container backed by this database.
func (s *Store) Container() *sqlstore.Container { return s.container }

// Close closes the database.
func (s *Store) Close() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.db.Close(); err != nil {
		return ioErr("close")
	}
	return nil
}

// Wipe removes the database and its sidecar files; missing files are fine.
func Wipe(dataDir string) error {
	path := filepath.Join(dataDir, DBName)
	for _, suffix := range []string{"", "-wal", "-shm", "-journal"} {
		if err := os.Remove(path + suffix); err != nil && !errors.Is(err, os.ErrNotExist) {
			return fmt.Errorf("e2ee store: wipe failed")
		}
	}
	return nil
}
