//go:build unix

package e2ee

import (
	"context"
	"os"
	"path/filepath"
	"syscall"
	"testing"

	"github.com/rs/zerolog"
)

// R-M5: with umask 0 every file SQLite creates must still be 0600 (messenger main never sets a umask).
func TestReviewModesUnderUmask0(t *testing.T) {
	old := syscall.Umask(0)
	defer syscall.Umask(old)
	dir := t.TempDir()
	s, err := Open(context.Background(), dir, zerolog.Nop())
	if err != nil {
		t.Fatal(err)
	}
	put(t, s, mkMsg("t1", 1), nil)
	for _, n := range []string{DBName, DBName + "-wal", DBName + "-shm"} {
		fi, err := os.Stat(filepath.Join(dir, n))
		if err != nil {
			t.Logf("%s: %v", n, err)
			continue
		}
		t.Logf("%s mode %o", n, fi.Mode().Perm())
		if fi.Mode().Perm() != 0o600 {
			t.Errorf("%s is %o, want 600", n, fi.Mode().Perm())
		}
	}
	_ = s.Close()
}
