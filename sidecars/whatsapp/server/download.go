package server

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"sync/atomic"

	"go.mau.fi/whatsmeow"

	"github.com/matpb/meta-mcp-rs/sidecars/whatsapp/api"
)

const (
	// maxDownloadBytes is the largest attachment the sidecar writes.
	maxDownloadBytes = 100 << 20
	// downloadSlack is what the encrypted stream adds to the media: up to 16 bytes of padding and a 10 byte MAC.
	downloadSlack = 32
)

var errTooLarge = errors.New("attachment too large")

// cappedFile is the whatsmeow.File the library decrypts into; it refuses to grow past limit and remembers that it did.
// It wraps the *os.File instead of embedding it, because an embedded file exposes ReadFrom and io.Copy would bypass Write.
type cappedFile struct {
	f     *os.File
	limit int64
	over  atomic.Bool
}

var _ whatsmeow.File = (*cappedFile)(nil)

func (c *cappedFile) deny() error {
	c.over.Store(true)
	return errTooLarge
}

func (c *cappedFile) Read(p []byte) (int, error)                { return c.f.Read(p) }
func (c *cappedFile) ReadAt(p []byte, off int64) (int, error)   { return c.f.ReadAt(p, off) }
func (c *cappedFile) Seek(off int64, whence int) (int64, error) { return c.f.Seek(off, whence) }
func (c *cappedFile) Stat() (os.FileInfo, error)                { return c.f.Stat() }

func (c *cappedFile) Write(p []byte) (int, error) {
	off, err := c.f.Seek(0, io.SeekCurrent)
	if err != nil {
		return 0, err
	}
	if off+int64(len(p)) > c.limit {
		return 0, c.deny()
	}
	return c.f.Write(p)
}

func (c *cappedFile) WriteAt(p []byte, off int64) (int, error) {
	if off < 0 || off+int64(len(p)) > c.limit {
		return 0, c.deny()
	}
	return c.f.WriteAt(p, off)
}

func (c *cappedFile) Truncate(size int64) error {
	if size > c.limit {
		return c.deny()
	}
	return c.f.Truncate(size)
}

// rewind empties the file and clears the over-limit flag for another attempt.
func (c *cappedFile) rewind() error {
	c.over.Store(false)
	if err := c.f.Truncate(0); err != nil {
		return err
	}
	_, err := c.f.Seek(0, io.SeekStart)
	return err
}

// destination is a checked dest_path: its open parent directory and the file name inside it.
// Writes go through the directory handle, so a symlink swapped in after the check cannot redirect them.
type destination struct {
	root *os.Root
	name string
}

const symlinkMsg = "dest_path must not be a symbolic link or sit in a symlinked directory"

func destError(msg string) *api.Error { return api.Errorf(api.CodeBadParams, "%s", msg) }

// openDestination checks that path is absolute, sits in an existing real directory and is not a symlink or a directory.
func openDestination(path string) (*destination, *api.Error) {
	if !filepath.IsAbs(path) {
		return nil, destError("dest_path must be an absolute file path")
	}
	dir, name := filepath.Dir(path), filepath.Base(path)
	if name == "." || name == ".." || name == string(filepath.Separator) {
		return nil, destError("dest_path must name a file")
	}
	dirInfo, err := os.Lstat(dir)
	switch {
	case err != nil || !dirInfo.IsDir() && dirInfo.Mode()&os.ModeSymlink == 0:
		return nil, destError("the directory of dest_path must exist")
	case dirInfo.Mode()&os.ModeSymlink != 0:
		return nil, destError(symlinkMsg)
	}
	root, err := os.OpenRoot(dir)
	if err != nil {
		return nil, destError("the directory of dest_path must exist")
	}
	opened, err := root.Stat(".")
	if err != nil || !os.SameFile(dirInfo, opened) {
		_ = root.Close()
		return nil, destError("the directory of dest_path changed while it was checked")
	}
	if fi, err := root.Lstat(name); err == nil {
		if fi.Mode()&os.ModeSymlink != 0 {
			_ = root.Close()
			return nil, destError(symlinkMsg)
		}
		if !fi.Mode().IsRegular() {
			_ = root.Close()
			return nil, destError("dest_path must be a regular file path")
		}
	} else if !errors.Is(err, fs.ErrNotExist) {
		_ = root.Close()
		return nil, api.Errorf(api.CodeIO, "cannot write the destination file")
	}
	return &destination{root: root, name: name}, nil
}

func (d *destination) close() { _ = d.root.Close() }

// newTemp creates an exclusive 0600 temp file next to the destination.
func (d *destination) newTemp() (*os.File, string, error) {
	var b [8]byte
	if _, err := rand.Read(b[:]); err != nil {
		return nil, "", err
	}
	name := ".dl-" + hex.EncodeToString(b[:]) + ".part"
	f, err := d.root.OpenFile(name, os.O_RDWR|os.O_CREATE|os.O_EXCL, 0o600)
	return f, name, err
}

// commit moves the finished temp file over the destination; the rename replaces the name itself, never a link target.
func (d *destination) commit(tmp string) error { return d.root.Rename(tmp, d.name) }

func (d *destination) discard(tmp string) { _ = d.root.Remove(tmp) }

// hashFile returns the size and sha256 of f, read from the start.
func hashFile(f *os.File) (int64, string, error) {
	if _, err := f.Seek(0, io.SeekStart); err != nil {
		return 0, "", err
	}
	h := sha256.New()
	n, err := io.Copy(h, f)
	if err != nil {
		return 0, "", err
	}
	return n, hex.EncodeToString(h.Sum(nil)), nil
}
