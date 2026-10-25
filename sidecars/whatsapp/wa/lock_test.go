package wa

import (
	"context"
	"errors"
	"testing"

	"github.com/rs/zerolog"
)

func TestOpenLocksDataDir(t *testing.T) {
	dir := t.TempDir()
	first, err := Open(context.Background(), dir, zerolog.Nop())
	if err != nil {
		t.Fatalf("first open: %v", err)
	}
	if _, err := Open(context.Background(), dir, zerolog.Nop()); !errors.Is(err, ErrLocked) {
		_ = first.Close()
		t.Fatalf("second open = %v, want ErrLocked", err)
	}
	if err := first.Close(); err != nil {
		t.Fatal(err)
	}
	again, err := Open(context.Background(), dir, zerolog.Nop())
	if err != nil {
		t.Fatalf("open after close: %v", err)
	}
	_ = again.Close()
}
