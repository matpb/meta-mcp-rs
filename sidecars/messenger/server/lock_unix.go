//go:build unix

package server

import (
	"errors"
	"os"
	"syscall"
)

// lockOpenFlags refuses to follow a symlink at the lock path.
const lockOpenFlags = os.O_CREATE | os.O_RDWR | syscall.O_NOFOLLOW

var errWouldBlock = syscall.EWOULDBLOCK

// tryLock takes a non-blocking exclusive flock; errWouldBlock means another process holds it.
func tryLock(f *os.File) error {
	err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB)
	if errors.Is(err, syscall.EWOULDBLOCK) {
		return errWouldBlock
	}
	return err
}

func unlock(f *os.File) {
	_ = syscall.Flock(int(f.Fd()), syscall.LOCK_UN)
}
