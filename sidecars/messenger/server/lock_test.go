package server

import (
	"errors"
	"testing"
)

func TestLockDataDirExclusive(t *testing.T) {
	dir := t.TempDir()
	first, err := lockDataDir(dir)
	if err != nil {
		t.Fatalf("first lock: %v", err)
	}
	if _, err := lockDataDir(dir); !errors.Is(err, errDirLocked) {
		first.release()
		t.Fatalf("second lock = %v, want errDirLocked", err)
	}
	first.release()
	again, err := lockDataDir(dir)
	if err != nil {
		t.Fatalf("lock after release: %v", err)
	}
	again.release()
}
