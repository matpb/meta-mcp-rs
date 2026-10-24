package server

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"syscall"
)

const lockFileName = "sidecar.lock"

// errDirLocked means another process holds the data_dir lock.
var errDirLocked = errors.New("server: data_dir is locked by another process")

// dirLock is the exclusive flock on data_dir/sidecar.lock, held until release or process exit.
type dirLock struct{ f *os.File }

// lockDataDir takes the lock without blocking; a second sidecar on the same data_dir gets errDirLocked.
func lockDataDir(dir string) (*dirLock, error) {
	f, err := os.OpenFile(filepath.Join(dir, lockFileName), os.O_CREATE|os.O_RDWR|syscall.O_NOFOLLOW, 0o600)
	if err != nil {
		return nil, fmt.Errorf("server: open lock file: %w", err)
	}
	_ = f.Chmod(0o600)
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		_ = f.Close()
		if errors.Is(err, syscall.EWOULDBLOCK) {
			return nil, errDirLocked
		}
		return nil, fmt.Errorf("server: lock data dir: %w", err)
	}
	return &dirLock{f: f}, nil
}

// release unlocks and closes the lock file; the file itself stays so a successor can lock it.
func (l *dirLock) release() {
	if l == nil || l.f == nil {
		return
	}
	_ = syscall.Flock(int(l.f.Fd()), syscall.LOCK_UN)
	_ = l.f.Close()
	l.f = nil
}
