//go:build windows

package server

import (
	"errors"
	"os"

	"golang.org/x/sys/windows"
)

// lockOpenFlags has no O_NOFOLLOW on Windows.
const lockOpenFlags = os.O_CREATE | os.O_RDWR

var errWouldBlock = windows.ERROR_LOCK_VIOLATION

// tryLock takes a non-blocking exclusive LockFileEx on byte 0; errWouldBlock means another process holds it.
func tryLock(f *os.File) error {
	err := windows.LockFileEx(windows.Handle(f.Fd()), windows.LOCKFILE_EXCLUSIVE_LOCK|windows.LOCKFILE_FAIL_IMMEDIATELY, 0, 1, 0, &windows.Overlapped{})
	if errors.Is(err, windows.ERROR_LOCK_VIOLATION) || errors.Is(err, windows.ERROR_IO_PENDING) {
		return errWouldBlock
	}
	return err
}

func unlock(f *os.File) {
	_ = windows.UnlockFileEx(windows.Handle(f.Fd()), 0, 1, 0, &windows.Overlapped{})
}
