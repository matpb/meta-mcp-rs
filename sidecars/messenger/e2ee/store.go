// Package e2ee owns the on-disk E2EE state: the whatsmeow device store and the encrypted-message cache.
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

const schemaVersion = 1

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

var dsnEscaper = strings.NewReplacer("%", "%25", "?", "%3f", "#", "%23")

func dsn(path string) string {
	return "file:" + dsnEscaper.Replace(path) +
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
