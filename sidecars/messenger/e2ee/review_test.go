package e2ee

import (
	"context"
	"os"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/rs/zerolog"
)

// R-M5: awkward data_dir names must land in that directory.
func TestReviewSpecialDirNames(t *testing.T) {
	base := t.TempDir()
	for _, name := range []string{"a b", "p%25q", "q?x", "h#i", "amp&x=y;z", "é", "100%", "%3f", "x%00y"} {
		if runtime.GOOS == "windows" && name == "q?x" {
			continue
		}
		dir := filepath.Join(base, name)
		if err := os.Mkdir(dir, 0o700); err != nil {
			t.Fatalf("mkdir %q: %v", name, err)
		}
		s, err := Open(context.Background(), dir, zerolog.Nop())
		if err != nil {
			t.Errorf("%q: open: %v", name, err)
			continue
		}
		put(t, s, mkMsg("t1", 1), nil)
		_ = s.Close()
		if _, err := os.Stat(filepath.Join(dir, DBName)); err != nil {
			t.Errorf("%q: db not in dir: %v", name, err)
		}
		ents, _ := os.ReadDir(base)
		for _, e := range ents {
			if e.Name() != name && !e.IsDir() {
				t.Errorf("%q: stray file %q in base", name, e.Name())
			}
		}
	}
}

// R-M5: hostile ids must be data, never SQL.
func TestReviewHostileIDs(t *testing.T) {
	dir := t.TempDir()
	s := openStore(t, dir)
	defer s.Close()
	evil := "x'; DROP TABLE sidecar_e2ee_messages; --"
	m := mkMsg(evil, 1)
	m.ID = "wa:" + evil + ":" + evil + ":" + evil
	put(t, s, m, nil)
	got, _, err := s.List(context.Background(), evil, nil, "", 10)
	if err != nil || len(got) != 1 {
		t.Fatalf("list: %v %v", got, err)
	}
	if _, found, err := s.Get(context.Background(), m.ID); err != nil || !found {
		t.Fatalf("get: %v %v", found, err)
	}
}

// R-M5: Open on an existing db is idempotent.
func TestReviewReopenIdempotent(t *testing.T) {
	dir := t.TempDir()
	for i := 0; i < 3; i++ {
		s := openStore(t, dir)
		put(t, s, mkMsg("t1", i+1), nil)
		_ = s.Close()
	}
	s := openStore(t, dir)
	defer s.Close()
	got, _, err := s.List(context.Background(), "t1", nil, "", 10)
	if err != nil || len(got) != 3 {
		t.Fatalf("after reopen: %d %v", len(got), err)
	}
}

// R-M5: Wipe removes only the db files.
func TestReviewWipeOnlyDB(t *testing.T) {
	dir := t.TempDir()
	s := openStore(t, dir)
	put(t, s, mkMsg("t1", 1), nil)
	_ = s.Close()
	keep := filepath.Join(dir, "threads.json")
	other := filepath.Join(dir, "unrelated.txt")
	_ = os.WriteFile(keep, []byte("{}"), 0o600)
	_ = os.WriteFile(other, []byte("x"), 0o600)
	if err := Wipe(dir); err != nil {
		t.Fatal(err)
	}
	ents, _ := os.ReadDir(dir)
	var names []string
	for _, e := range ents {
		names = append(names, e.Name())
	}
	t.Logf("left after Wipe: %v", names)
	for _, n := range names {
		if n == DBName || n == DBName+"-wal" || n == DBName+"-shm" {
			t.Errorf("%s survived Wipe", n)
		}
	}
}
